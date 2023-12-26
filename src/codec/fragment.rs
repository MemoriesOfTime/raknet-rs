use std::cmp::Reverse;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::BufMut;
use futures::{Sink, Stream};
use lru::LruCache;
use pin_project_lite::pin_project;
use priority_queue::PriorityQueue;
use tracing::debug;

use crate::codec::PollPacket;
use crate::errors::CodecError;
use crate::packet::connected::{Fragment, Frame};
use crate::packet::{connected, PackId, Packet};

pin_project! {
    /// Defragment the frame set packet from stream (UdpFramed). Enable external consumption of
    /// continuous frame set packets.
    /// Notice that packets stream must pass this layer first, then go to the ack layer and timeout layer.
    /// Because this layer could abort the frames in frame set packet, and the ack layer will promise
    /// the client that we have received the frame set packet. Moreover, this layer needs to get the
    /// Disconnect packet sent by the timeout layer to clear the parted cache.
    pub(super) struct DeFragment<F> {
        #[pin]
        frame: F,
        // limit the max size of a parted frames set, 0 means no limit
        // it will abort the split frame if the parted_size reaches limit.
        limit_size: u32,
        // limit the max count of all parted frames sets from an address
        // it might cause client resending frames if the limit is reached.
        limit_parted: usize,
        // parts helper. LruCache used to protect from causing OOM due to malicious
        // users sending a large number of parted IDs.
        parts: HashMap<SocketAddr, LruCache<u16, PriorityQueue<Frame, Reverse<u32>>>>,
    }
}

pub(super) trait DeFragmented: Sized {
    fn defragmented(self, limit_size: u32, limit_parted: usize) -> DeFragment<Self>;
}

impl<T> DeFragmented for T
where
    T: Stream<Item = Result<(Packet, SocketAddr), CodecError>>
        + Sink<(Packet, SocketAddr), Error = CodecError>,
{
    fn defragmented(self, limit_size: u32, limit_parted: usize) -> DeFragment<Self> {
        DeFragment {
            frame: self,
            limit_size,
            limit_parted,
            parts: HashMap::new(),
        }
    }
}

impl<F> Stream for DeFragment<F>
where
    F: Stream<Item = Result<(Packet, SocketAddr), CodecError>>,
{
    type Item = Result<(Packet, SocketAddr), CodecError>;

    // TODO
    // Splitted frames in a FrameSet are usually ordered, so use Vec instead of PriorityQueue
    // might be better, we should have a benchmark.
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        loop {
            let (packet, addr) = match this.frame.as_mut().poll_packet(cx) {
                Ok(v) => v,
                Err(poll) => return poll,
            };

            let Packet::Connected(connected::Packet::FrameSet(frame_set)) = packet else {
                return Poll::Ready(Some(Ok((packet, addr))));
            };

            let mut frames = None;
            let frames_len = frame_set.frames.len();
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
                    let parts = this.parts.entry(addr).or_insert_with(|| {
                        LruCache::new(
                            NonZeroUsize::new(*this.limit_parted).expect("limit_parted > 0"),
                        )
                    });
                    let frames_queue = parts.get_or_insert_mut(parted_id, || {
                        debug!("new parted_id {parted_id} from {addr}");
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
                    let acc_frame: Frame = parts
                        .pop(&parted_id)
                        .unwrap_or_else(|| {
                            unreachable!("parted_id {parted_id} should be set before")
                        })
                        .into_sorted_iter()
                        .map(|(f, _)| f)
                        .reduce(|mut acc, next| {
                            // merge all parted frames
                            acc.body.put(next.body);
                            // remove the fragment info to keep hash of this frame normal
                            acc.fragment = None;
                            acc
                        })
                        .expect("there is at least one frame");
                    frames
                        .get_or_insert_with(|| Vec::with_capacity(frames_len))
                        .push(acc_frame);
                    continue;
                }
                frames
                    .get_or_insert_with(|| Vec::with_capacity(frames_len))
                    .push(frame);
            }
            if let Some(frames) = frames {
                return Poll::Ready(Some(Ok((
                    Packet::Connected(connected::Packet::FrameSet(connected::FrameSet {
                        frames,
                        ..frame_set
                    })),
                    addr,
                ))));
            }
        }
    }
}

impl<F> Sink<(Packet, SocketAddr)> for DeFragment<F>
where
    F: Sink<(Packet, SocketAddr), Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_ready(cx)
    }

    fn start_send(
        self: Pin<&mut Self>,
        (packet, addr): (Packet, SocketAddr),
    ) -> Result<(), Self::Error> {
        let this = self.project();
        if let Packet::Connected(connected::Packet::FrameSet(frame_set)) = &packet {
            if matches!(frame_set.inner_pack_id()?, PackId::DisconnectNotification) {
                debug!("disconnect from {}, clean it's frame parts buffer", addr);
                this.parts.remove(&addr);
            }
        };
        this.frame.start_send((packet, addr))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_close(cx)
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use bytes::BytesMut;
    use futures::StreamExt;
    use futures_async_stream::stream;
    use rand::seq::SliceRandom;

    use super::DeFragment;
    use crate::errors::CodecError;
    use crate::packet::connected::{self, Flags, Fragment, Frame, FrameSet, Uint24le};
    use crate::packet::Packet;

    fn frame_set<'a, T: AsRef<str> + 'a>(
        idx: impl IntoIterator<Item = &'a (u32, u16, u32, T)>,
    ) -> Packet {
        Packet::Connected(connected::Packet::FrameSet(FrameSet {
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
        }))
    }

    fn no_frag_frame_set<'a>(bodies: impl IntoIterator<Item = &'a str>) -> Packet {
        Packet::Connected(connected::Packet::FrameSet(FrameSet {
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
        }))
    }

    #[tokio::test]
    async fn test_defragment_works() {
        let addr1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let frame = {
            #[stream]
            async {
                yield (
                    frame_set([
                        &(5, 7, 0, "h"),
                        &(5, 7, 3, "p"),
                        &(5, 7, 4, "y"),
                        &(5, 6, 4, "k"),
                    ]),
                    addr1,
                );
                yield (
                    frame_set([&(5, 11, 0, "f"), &(5, 11, 3, "n"), &(5, 11, 4, "y")]),
                    addr2,
                );

                yield (
                    frame_set([&(5, 7, 2, "p"), &(5, 7, 1, "a"), &(5, 7, 4, "y")]),
                    addr1,
                );
                yield (
                    frame_set([&(5, 11, 2, "n"), &(5, 11, 1, "u"), &(5, 11, 0, "f")]),
                    addr2,
                );
            }
        };

        tokio::pin!(frame);
        let mut frag = DeFragment {
            frame: frame.map(Ok),
            limit_size: 0,
            limit_parted: 512,
            parts: HashMap::new(),
        };

        {
            let (set, addr) = frag.next().await.unwrap().unwrap();
            assert_eq!(addr, addr1);

            let Packet::Connected(connected::Packet::FrameSet(set)) = set else {
                panic!("should be a frameset")
            };

            // frames should be merged
            assert_eq!(set.frames.len(), 1);
            assert_eq!(
                String::from_utf8(set.frames[0].body.to_vec()).unwrap(),
                "happy"
            );
            // wiped
            assert!(set.frames[0].fragment.is_none());
        }

        {
            let (set, addr) = frag.next().await.unwrap().unwrap();
            assert_eq!(addr, addr2);

            let Packet::Connected(connected::Packet::FrameSet(set)) = set else {
                panic!("should be a frameset")
            };
            assert_eq!(set.frames.len(), 1);
            assert_eq!(
                String::from_utf8(set.frames[0].body.to_vec()).unwrap(),
                "funny"
            );
            // wiped
            assert!(set.frames[0].fragment.is_none());
        }

        // could only be polled twice
        assert!(frag.next().await.is_none());
    }

    #[tokio::test]
    async fn test_defragment_bad_parted_index() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let frame = {
            #[stream]
            async {
                yield (frame_set([&(10, 7, 10, "h")]), addr);
                yield (frame_set([&(22, 7, 6, "h")]), addr);
            }
        };

        tokio::pin!(frame);
        let mut frag = DeFragment {
            frame: frame.map(Ok),
            limit_size: 20,
            limit_parted: 512,
            parts: HashMap::new(),
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
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let frame = {
            #[stream]
            async {
                yield (frame_set([&(3, 0, 0, "0")]), addr);
                yield (frame_set([&(3, 1, 0, "0")]), addr);
                yield (frame_set([&(3, 2, 0, "0")]), addr); // 3rd one will motivate lru to drop 1st one

                yield (frame_set([&(3, 0, 1, "1")]), addr);
                yield (frame_set([&(3, 0, 2, "2")]), addr); // cannot collect parted_id 0

                yield (frame_set([&(3, 2, 2, "2")]), addr);
            }
        };

        tokio::pin!(frame);
        let mut frag = DeFragment {
            frame: frame.map(Ok),
            limit_size: 0,
            limit_parted: 2,
            parts: HashMap::new(),
        };

        assert!(frag.next().await.is_none());
        let lru = frag.parts.get(&addr).unwrap();
        assert_eq!(lru.len(), 2);
        assert_eq!(lru.peek(&0).unwrap().len(), 2);
        assert_eq!(lru.peek(&2).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_defragment_mixed() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let frame = {
            #[stream]
            async {
                yield (
                    frame_set([
                        &(5, 7, 0, "h"),
                        &(5, 7, 3, "p"),
                        &(5, 7, 4, "y"),
                        &(5, 6, 4, "k"),
                    ]),
                    addr,
                );
                yield (no_frag_frame_set(["funny"]), addr);
                yield (
                    frame_set([&(5, 7, 2, "p"), &(5, 7, 1, "a"), &(5, 7, 4, "y")]),
                    addr,
                );
            }
        };

        tokio::pin!(frame);
        let mut frag = DeFragment {
            frame: frame.map(Ok),
            limit_size: 0,
            limit_parted: 2,
            parts: HashMap::new(),
        };

        {
            let (set, _) = frag.next().await.unwrap().unwrap();
            let Packet::Connected(connected::Packet::FrameSet(set)) = set else {
                panic!("should be a frameset")
            };
            assert_eq!(set.frames.len(), 1);
            assert_eq!(
                String::from_utf8(set.frames[0].body.to_vec()).unwrap(),
                "funny"
            );
        }

        {
            let (set, _) = frag.next().await.unwrap().unwrap();
            let Packet::Connected(connected::Packet::FrameSet(set)) = set else {
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
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
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
                    yield (frame_set(chunk), addr);
                }
            }
        };

        tokio::pin!(frame);
        let mut frag = DeFragment {
            frame: frame.map(Ok),
            limit_size: 0,
            limit_parted: 2,
            parts: HashMap::new(),
        };

        let (set, _) = frag.next().await.unwrap().unwrap();
        let Packet::Connected(connected::Packet::FrameSet(set)) = set else {
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
