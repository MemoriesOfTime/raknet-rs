use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::BufMut;
use fastrace::{Event, Span};
use futures::{ready, Stream, StreamExt};
use lru::LruCache;
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::link::SharedLink;
use crate::packet::connected::{Fragment, Frame, FrameMut, FrameSet, FramesMut};

const DEFAULT_DEFRAGMENT_BUF_SIZE: usize = 512;

/// Frame parts belonging to a same parted id
struct FramePart {
    parted_index: Reverse<u32>,
    frame: FrameMut,
}

impl PartialEq for FramePart {
    fn eq(&self, other: &Self) -> bool {
        self.parted_index == other.parted_index
    }
}

impl Eq for FramePart {}

impl PartialOrd for FramePart {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FramePart {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.parted_index.cmp(&other.parted_index)
    }
}

pin_project! {
    /// Defragment the frame set packet from stream [`UdpFramed`]. Enable external consumption of
    /// continuous frame set packets.
    pub(crate) struct DeFragment<F> {
        #[pin]
        frame: F,
        // limit the max size of a parted frames set, 0 means no limit
        // it will abort the split frame if the parted_size reaches limit.
        limit_size: u32,
        // reassemble parts helper. [`LruCache`] used to protect from causing OOM due to malicious
        // users sending a large number of parted IDs.
        parts: LruCache<u16, BinaryHeap<FramePart>>,
        buffer: VecDeque<FrameSet<Frame>>,
        link: SharedLink,
        span: Option<Span>,
    }
}

pub(crate) trait DeFragmented: Sized {
    fn defragmented(
        self,
        limit_size: u32,
        limit_parted: usize,
        link: SharedLink,
    ) -> DeFragment<Self>;
}

impl<F> DeFragmented for F
where
    F: Stream<Item = Result<FrameSet<FramesMut>, CodecError>>,
{
    fn defragmented(
        self,
        limit_size: u32,
        limit_parted: usize,
        link: SharedLink,
    ) -> DeFragment<Self> {
        DeFragment {
            frame: self,
            limit_size,
            parts: LruCache::new(NonZeroUsize::new(limit_parted).expect("limit_parted > 0")),
            buffer: VecDeque::with_capacity(DEFAULT_DEFRAGMENT_BUF_SIZE),
            link,
            span: None,
        }
    }
}

impl<F> Stream for DeFragment<F>
where
    F: Stream<Item = Result<FrameSet<FramesMut>, CodecError>>,
{
    type Item = Result<FrameSet<Frame>, CodecError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            // empty buffer
            if let Some(frame_set) = this.buffer.pop_front() {
                this.span.take();
                return Poll::Ready(Some(Ok(frame_set)));
            }
            let Some(frame_set) = ready!(this.frame.poll_next_unpin(cx)?) else {
                return Poll::Ready(None);
            };
            this.span.get_or_insert_with(|| {
                Span::enter_with_local_parent("codec.defragment").with_properties(|| {
                    [
                        ("buffer_size", this.buffer.len().to_string()),
                        ("pending", this.parts.len().to_string()),
                    ]
                })
            });
            for frame in frame_set.set {
                if let Some(Fragment {
                    parted_size,
                    parted_id,
                    parted_index,
                }) = frame.fragment
                {
                    // promise that parted_index is always less than parted_size
                    if parted_index >= parted_size {
                        // perhaps network bit-flips
                        this.link.outgoing_nack(frame_set.seq_num);
                        let err = format!(
                            "parted_index {} >= parted_size {}",
                            parted_index, parted_size
                        );
                        Event::add_to_local_parent(err.clone(), || []);
                        return Poll::Ready(Some(Err(CodecError::PartedFrame(err))));
                    }
                    if *this.limit_size != 0 && parted_size > *this.limit_size {
                        let err = format!(
                            "parted_size {} exceed limit_size {}",
                            parted_size, *this.limit_size
                        );
                        Event::add_to_local_parent(err.clone(), || []);
                        return Poll::Ready(Some(Err(CodecError::PartedFrame(err))));
                    }
                    let frames_queue = this.parts.get_or_insert_mut(parted_id, || {
                        // init the PriorityQueue with the capacity defined by user.
                        BinaryHeap::with_capacity(parted_size as usize)
                    });
                    frames_queue.push(FramePart {
                        parted_index: Reverse(parted_index),
                        frame,
                    });
                    if frames_queue.len() < parted_size as usize {
                        continue;
                    }
                    // parted_index is always less than parted_size, frames_queue length
                    // reaches parted_size and frame is hashed by parted_index, so here we
                    // get the complete frames vector
                    let merged_frame: Frame = this
                        .parts
                        .pop(&parted_id)
                        .expect("parted_id should be set before")
                        .into_iter_sorted()
                        .map(|part| part.frame)
                        .reduce(|mut acc, next| {
                            // merge all parted frames
                            acc.body.put(next.body);
                            acc
                        })
                        .expect("there is at least one frame")
                        .reassembled()
                        .freeze();

                    this.buffer.push_back(FrameSet {
                        seq_num: frame_set.seq_num,
                        set: merged_frame,
                    });
                    continue;
                }
                this.buffer.push_back(FrameSet {
                    seq_num: frame_set.seq_num,
                    set: frame.freeze(),
                });
            }
        }
    }
}

#[cfg(test)]
mod test {
    use bytes::BytesMut;
    use futures::StreamExt;
    use futures_async_stream::stream;
    use rand::seq::SliceRandom;

    use super::*;
    use crate::errors::CodecError;
    use crate::link::TransferLink;
    use crate::packet::connected::{Flags, Fragment, Frame, FrameSet, FramesMut};

    fn frame_set<'a, T: AsRef<str> + 'a>(
        idx: impl IntoIterator<Item = &'a (u32, u16, u32, T)>,
    ) -> FrameSet<FramesMut> {
        FrameSet {
            seq_num: 0.into(),
            set: idx
                .into_iter()
                .map(|(parted_size, parted_id, parted_index, body)| Frame {
                    flags: Flags::parse(0b011_11100),
                    reliable_frame_index: None,
                    seq_frame_index: None,
                    ordered: None,
                    fragment: Some(Fragment {
                        parted_size: *parted_size,
                        parted_id: *parted_id,
                        parted_index: *parted_index,
                    }),
                    body: BytesMut::from(body.as_ref()),
                })
                .collect(),
        }
    }

    fn no_frag_frame_set<'a>(bodies: impl IntoIterator<Item = &'a str>) -> FrameSet<FramesMut> {
        FrameSet {
            seq_num: 0.into(),
            set: bodies
                .into_iter()
                .map(|body| Frame {
                    flags: Flags::parse(0b011_11100),
                    reliable_frame_index: None,
                    seq_frame_index: None,
                    ordered: None,
                    fragment: None,
                    body: BytesMut::from(body),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn test_defragment_works() {
        let frame = {
            #[stream]
            async {
                yield frame_set([
                    &(5, 7, 0, "h"),
                    &(5, 7, 3, "p"),
                    &(5, 7, 4, "y"),
                    &(5, 6, 4, "k"),
                ]);
                yield frame_set([&(5, 7, 2, "p"), &(5, 7, 1, "a"), &(5, 7, 4, "y")]);
            }
        };

        tokio::pin!(frame);
        let mut frag = frame.map(Ok).defragmented(
            0,
            512,
            TransferLink::new_arc(crate::RoleContext::test_server()),
        );
        let set = frag.next().await.unwrap().unwrap();

        // frames should be merged
        assert_eq!(String::from_utf8(set.set.body.to_vec()).unwrap(), "happy");
        // fragment part was wiped
        assert!(!set.set.flags.parted);
        assert!(set.set.fragment.is_none());

        // could only be polled once
        assert!(frag.next().await.is_none());
    }

    #[tokio::test]
    async fn test_defragment_bad_parted_index() {
        let frame = {
            #[stream]
            async {
                yield frame_set([&(10, 7, 10, "h")]);
                yield frame_set([&(22, 7, 6, "h")]);
            }
        };
        tokio::pin!(frame);
        let mut frag = frame.map(Ok).defragmented(
            20,
            512,
            TransferLink::new_arc(crate::RoleContext::test_server()),
        );
        assert!(matches!(
            frag.next().await.unwrap(),
            Err(CodecError::PartedFrame(..))
        ));
        assert!(matches!(
            frag.next().await.unwrap(),
            Err(CodecError::PartedFrame(..))
        ));
        assert!(frag.next().await.is_none());
    }

    #[tokio::test]
    async fn test_defragment_lru_dropped() {
        let frame = {
            #[stream]
            async {
                yield frame_set([&(3, 0, 0, "0")]);
                yield frame_set([&(3, 1, 0, "0")]);
                yield frame_set([&(3, 2, 0, "0")]); // 3rd one will motivate lru to drop 1st one

                yield frame_set([&(3, 0, 1, "1")]);
                yield frame_set([&(3, 0, 2, "2")]); // cannot collect parted_id 0

                yield frame_set([&(3, 2, 2, "2")]);
            }
        };

        tokio::pin!(frame);
        let mut frag = frame.map(Ok).defragmented(
            0,
            2,
            TransferLink::new_arc(crate::RoleContext::test_server()),
        );
        assert!(frag.next().await.is_none());
        assert_eq!(frag.parts.len(), 2);
        assert_eq!(frag.parts.peek(&0).unwrap().len(), 2);
        assert_eq!(frag.parts.peek(&2).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_defragment_mixed() {
        let frame = {
            #[stream]
            async {
                yield frame_set([
                    &(5, 7, 0, "h"),
                    &(5, 7, 3, "p"),
                    &(5, 7, 4, "y"),
                    &(5, 6, 4, "k"),
                ]);
                yield no_frag_frame_set(["funny"]);
                yield frame_set([&(5, 7, 2, "p"), &(5, 7, 1, "a"), &(5, 7, 4, "y")]);
            }
        };

        tokio::pin!(frame);
        let mut frag = frame.map(Ok).defragmented(
            0,
            2,
            TransferLink::new_arc(crate::RoleContext::test_server()),
        );

        {
            let set = frag.next().await.unwrap().unwrap();
            assert_eq!(String::from_utf8(set.set.body.to_vec()).unwrap(), "funny");
        }

        {
            let set = frag.next().await.unwrap().unwrap();
            assert_eq!(String::from_utf8(set.set.body.to_vec()).unwrap(), "happy");
        }
    }

    async fn test_defragment_fuzzing_with_scale(scale: usize) {
        let mut parted_slice = (0..scale).collect::<Vec<_>>();
        let final_body = parted_slice
            .iter()
            .fold(String::new(), |acc, next| format!("{acc}{next}"));

        parted_slice.shuffle(&mut rand::thread_rng());

        let frame = {
            #[stream]
            async {
                let chunk_size = rand::random::<usize>() % scale + 1; // non zero
                for chunk in std::iter::repeat((scale as u32, 0))
                    .zip(parted_slice)
                    .map(|(l, r)| (l.0, l.1, r as u32, r.to_string()))
                    .collect::<Vec<_>>()
                    .chunks(chunk_size)
                {
                    yield frame_set(chunk);
                }
            }
        };

        tokio::pin!(frame);
        let mut frag = frame.map(Ok).defragmented(
            0,
            1,
            TransferLink::new_arc(crate::RoleContext::test_server()),
        );

        let set = frag.next().await.unwrap().unwrap();
        assert_eq!(
            String::from_utf8(set.set.body.to_vec()).unwrap(),
            final_body
        );
    }

    #[tokio::test]
    async fn test_defragment_fuzzing_with_scale_10() {
        test_defragment_fuzzing_with_scale(10).await;
    }

    #[tokio::test]
    async fn test_defragment_fuzzing_with_scale_100() {
        test_defragment_fuzzing_with_scale(100).await;
    }

    #[tokio::test]
    async fn test_defragment_fuzzing_with_scale_1000() {
        test_defragment_fuzzing_with_scale(1000).await;
    }

    #[tokio::test]
    async fn test_defragment_fuzzing_with_scale_10000() {
        test_defragment_fuzzing_with_scale(10000).await;
    }
}
