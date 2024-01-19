use std::cmp::Reverse;
use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{BufMut, Bytes, BytesMut};
use futures::{ready, Stream, StreamExt};
use lru::LruCache;
use pin_project_lite::pin_project;
use priority_queue::PriorityQueue;

use crate::errors::CodecError;
use crate::packet::connected::{self, Fragment, Frame, FrameSet};

const DEFAULT_DEFRAGMENT_BUF_SIZE: usize = 512;

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
        parts: LruCache<u16, PriorityQueue<Frame<BytesMut>, Reverse<u32>>>,
        buffer: VecDeque<FrameSet<Bytes>>,
    }
}

pub(super) trait DeFragmented: Sized {
    fn defragmented(self, limit_size: u32, limit_parted: usize) -> DeFragment<Self>;
}

impl<F> DeFragmented for F {
    fn defragmented(self, limit_size: u32, limit_parted: usize) -> DeFragment<Self> {
        DeFragment {
            frame: self,
            limit_size,
            parts: LruCache::new(NonZeroUsize::new(limit_parted).expect("limit_parted > 0")),
            buffer: VecDeque::with_capacity(DEFAULT_DEFRAGMENT_BUF_SIZE),
        }
    }
}

impl<F> Stream for DeFragment<F>
where
    F: Stream<Item = Result<connected::Packet<BytesMut>, CodecError>>,
{
    type Item = Result<connected::Packet<Bytes>, CodecError>;

    // TODO
    // Splitted frames in a FrameSet are usually ordered, so use Vec instead of PriorityQueue
    // might be better, we should have a benchmark.
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        loop {
            // empty buffer
            if let Some(pack) = this.buffer.pop_front() {
                return Poll::Ready(Some(Ok(connected::Packet::FrameSet(pack))));
            }

            let Some(packet) = ready!(this.frame.poll_next_unpin(cx)?) else {
                return Poll::Ready(None);
            };

            let connected::Packet::FrameSet(frame_set) = packet else {
                return Poll::Ready(Some(Ok(packet.freeze())));
            };

            for frame in frame_set.frames {
                if let Some(Fragment {
                    parted_size,
                    parted_id,
                    parted_index,
                }) = frame.fragment.clone()
                {
                    // promise that parted_index is always less than parted_size
                    if parted_index >= parted_size {
                        return Poll::Ready(Some(Err(CodecError::PartedFrame(format!(
                            "parted_index {} >= parted_size {}",
                            parted_index, parted_size
                        )))));
                    }
                    if *this.limit_size != 0 && parted_size > *this.limit_size {
                        return Poll::Ready(Some(Err(CodecError::PartedFrame(format!(
                            "parted_size {} exceed limit_size {}",
                            parted_size, *this.limit_size
                        )))));
                    }

                    let frames_queue = this.parts.get_or_insert_mut(parted_id, || {
                        // init the PriorityQueue with the capacity defined by user.
                        PriorityQueue::with_capacity(parted_size as usize)
                    });
                    frames_queue.push(frame, Reverse(parted_index));
                    if frames_queue.len() < parted_size as usize {
                        continue;
                    }
                    // parted_index is always less than parted_size, frames_queue length
                    // reaches parted_size and frame is hashed by parted_index, so here we
                    // get the complete frames vector
                    let acc_frame: Frame<Bytes> = this
                        .parts
                        .pop(&parted_id)
                        .unwrap_or_else(|| {
                            unreachable!("parted_id {parted_id} should be set before")
                        })
                        .into_sorted_iter()
                        .map(|(f, _)| f)
                        .reduce(|mut acc, next| {
                            // merge all parted frames
                            acc.body.put(next.body);
                            acc.reassembled();
                            acc
                        })
                        .expect("there is at least one frame")
                        .freeze();

                    // TODO: optimize vec![] to single frame set
                    this.buffer.push_back(FrameSet {
                        seq_num: frame_set.seq_num,
                        frames: vec![acc_frame],
                    });
                    continue;
                }
                this.buffer.push_back(FrameSet {
                    seq_num: frame_set.seq_num,
                    frames: vec![frame.freeze()],
                });
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::VecDeque;
    use std::num::NonZeroUsize;

    use bytes::BytesMut;
    use futures::StreamExt;
    use futures_async_stream::stream;
    use lru::LruCache;
    use rand::seq::SliceRandom;

    use super::DeFragment;
    use crate::errors::CodecError;
    use crate::packet::connected::{self, Flags, Fragment, Frame, FrameSet, Uint24le};

    fn frame_set<'a, T: AsRef<str> + 'a>(
        idx: impl IntoIterator<Item = &'a (u32, u16, u32, T)>,
    ) -> connected::Packet<BytesMut> {
        connected::Packet::FrameSet(FrameSet {
            seq_num: Uint24le(0),
            frames: idx
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
        })
    }

    fn no_frag_frame_set<'a>(
        bodies: impl IntoIterator<Item = &'a str>,
    ) -> connected::Packet<BytesMut> {
        connected::Packet::FrameSet(FrameSet {
            seq_num: Uint24le(0),
            frames: bodies
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
        })
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
        let mut frag = DeFragment {
            frame: frame.map(Ok),
            limit_size: 0,
            parts: LruCache::new(NonZeroUsize::new(512).expect("limit_parted > 0")),
            buffer: VecDeque::new(),
        };

        let set = frag.next().await.unwrap().unwrap();
        let connected::Packet::FrameSet(set) = set else {
            panic!("should be a frameset")
        };

        // frames should be merged
        assert_eq!(set.frames.len(), 1);
        assert_eq!(
            String::from_utf8(set.frames[0].body.to_vec()).unwrap(),
            "happy"
        );
        // wiped
        assert!(!set.frames[0].flags.parted());
        assert!(set.frames[0].fragment.is_none());

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
        let mut frag = DeFragment {
            frame: frame.map(Ok),
            limit_size: 20,
            parts: LruCache::new(NonZeroUsize::new(512).expect("limit_parted > 0")),
            buffer: VecDeque::new(),
        };

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
        let mut frag = DeFragment {
            frame: frame.map(Ok),
            limit_size: 0,
            parts: LruCache::new(NonZeroUsize::new(2).expect("limit_parted > 0")),
            buffer: VecDeque::new(),
        };

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
        let mut frag = DeFragment {
            frame: frame.map(Ok),
            limit_size: 0,
            parts: LruCache::new(NonZeroUsize::new(2).expect("limit_parted > 0")),
            buffer: VecDeque::new(),
        };

        {
            let set = frag.next().await.unwrap().unwrap();
            let connected::Packet::FrameSet(set) = set else {
                panic!("should be a frameset")
            };
            assert_eq!(set.frames.len(), 1);
            assert_eq!(
                String::from_utf8(set.frames[0].body.to_vec()).unwrap(),
                "funny"
            );
        }

        {
            let set = frag.next().await.unwrap().unwrap();
            let connected::Packet::FrameSet(set) = set else {
                panic!("should be a frameset")
            };
            assert_eq!(set.frames.len(), 1);
            assert_eq!(
                String::from_utf8(set.frames[0].body.to_vec()).unwrap(),
                "happy"
            );
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
        let mut frag = DeFragment {
            frame: frame.map(Ok),
            limit_size: 0,
            parts: LruCache::new(NonZeroUsize::new(1).expect("limit_parted > 0")),
            buffer: VecDeque::new(),
        };

        let set = frag.next().await.unwrap().unwrap();
        let connected::Packet::FrameSet(set) = set else {
            panic!("should be a frameset")
        };
        assert_eq!(set.frames.len(), 1);
        assert_eq!(
            String::from_utf8(set.frames[0].body.to_vec()).unwrap(),
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
