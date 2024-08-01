use std::cmp::min;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Buf;
use futures::Sink;
use pin_project_lite::pin_project;

use crate::packet::connected::{self, Flags, Frame, Ordered};
use crate::packet::{FRAGMENT_PART_SIZE, FRAME_SET_HEADER_SIZE};
use crate::utils::u24;
use crate::{Message, Reliability};

pin_project! {
    // Fragment splits the message into multiple frames if the message is too large.
    pub(crate) struct Fragment<F> {
        #[pin]
        frame: F,
        mtu: usize,
        reliable_write_index: u24,
        order_write_index: Vec<u24>,
        parted_id_write: u16,
    }
}

pub(crate) trait Fragmented: Sized {
    fn fragmented(self, mtu: usize, max_channels: usize) -> Fragment<Self>;
}

impl<F> Fragmented for F
where
    F: Sink<Frame, Error = io::Error>,
{
    fn fragmented(self, mtu: usize, max_channels: usize) -> Fragment<Self> {
        Fragment {
            frame: self,
            mtu,
            reliable_write_index: 0.into(),
            order_write_index: std::iter::repeat(0.into()).take(max_channels).collect(),
            parted_id_write: 0,
        }
    }
}

impl<F> Sink<Message> for Fragment<F>
where
    F: Sink<Frame, Error = io::Error>,
{
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, msg: Message) -> Result<(), Self::Error> {
        let mut this = self.project();
        let mut reliability = msg.get_reliability();
        let order_channel = msg.get_order_channel() as usize;

        debug_assert!(
            order_channel < this.order_write_index.len(),
            "sink a message with too large order channel {order_channel}, max channels {}",
            this.order_write_index.len()
        );

        let mut body = msg.into_data();

        // max_len is the maximum size of the frame body (excluding the fragment part option)
        let mut max_len = *this.mtu - FRAME_SET_HEADER_SIZE - reliability.size();

        if body.len() > max_len {
            // adjust reliability when packet needs splitting
            reliability = match reliability {
                Reliability::Unreliable => Reliability::Reliable,
                Reliability::UnreliableSequenced => Reliability::ReliableSequenced,
                Reliability::UnreliableWithAckReceipt => Reliability::ReliableWithAckReceipt,
                _ => reliability,
            };
        }

        // calculate again as we may have adjusted reliability
        max_len = *this.mtu - FRAME_SET_HEADER_SIZE - reliability.size();

        // get reliable_frame_index and ordered part for each frame
        let mut indices_for_frame = || {
            // reliable_frame_index performs for each frame to ensure it is not duplicated
            let reliable_frame_index = reliability.is_reliable().then(|| {
                let index = *this.reliable_write_index;
                *this.reliable_write_index += 1;
                index
            });
            // Ordered performs across all fragmented frames to ensure that the entire data is
            // received in the same order as it was sent.
            let ordered = reliability.is_sequenced_or_ordered().then_some(Ordered {
                frame_index: this.order_write_index[order_channel],
                channel: order_channel as u8,
            });
            (reliable_frame_index, ordered)
        };

        if body.len() <= max_len {
            // not exceeding the mss, no need to split.
            let (reliable_frame_index, ordered) = indices_for_frame();
            if reliability.is_sequenced_or_ordered() {
                this.order_write_index[order_channel] += 1;
            }
            let frame = Frame {
                flags: Flags::new(reliability, false),
                reliable_frame_index,
                seq_frame_index: None,
                ordered,
                fragment: None,
                body,
            };
            return this.frame.start_send(frame);
        }

        // subtract the fragment part option size
        let per_len = max_len - FRAGMENT_PART_SIZE;
        let parted_size = body.len().div_ceil(per_len) as u32;
        let parted_id = *this.parted_id_write;
        *this.parted_id_write = this.parted_id_write.wrapping_add(1);

        // split the data
        for parted_index in 0..parted_size {
            let (reliable_frame_index, ordered) = indices_for_frame();
            let frame = Frame {
                flags: Flags::new(reliability, true),
                reliable_frame_index,
                seq_frame_index: None,
                ordered,
                fragment: Some(connected::Fragment {
                    parted_size,
                    parted_id,
                    parted_index,
                }),
                body: body.split_to(min(per_len, body.len())),
            };
            debug_assert!(
                frame.body.len() <= max_len,
                "split failed, the frame body is too large"
            );
            // FIXME: poll_ready is not ensured before send. But it is ok because the next
            // layer has buffer(ie. next_frame.start_send will always return Ok, and never mess up
            // data)
            this.frame.as_mut().start_send(frame)?;
        }

        if reliability.is_sequenced_or_ordered() {
            this.order_write_index[order_channel] += 1;
        }

        debug_assert!(
            body.remaining() == 0,
            "split failed, there still remains data"
        );

        Ok(())
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
    use bytes::Bytes;
    use connected::Frames;

    use super::*;

    #[derive(Debug, Default)]
    struct DstSink {
        buf: Frames,
    }

    impl Sink<Frame> for DstSink {
        type Error = io::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, item: Frame) -> Result<(), Self::Error> {
            self.buf.push(item);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn test_fragmented_works() {
        let dst = DstSink::default().fragmented(50, 8);
        tokio::pin!(dst);
        // 1
        dst.as_mut()
            .start_send(Message::new(
                Reliability::ReliableOrdered,
                0,
                Bytes::from_static(b"hello world"),
            ))
            .unwrap();
        // 2
        dst.as_mut()
            .start_send(Message::new(
                Reliability::ReliableOrdered,
                1,
                Bytes::from_static(b"hello world, hello world, hello world, hello world"),
            ))
            .unwrap();
        // 1
        dst.as_mut()
            .start_send(Message::new(
                Reliability::Reliable,
                0,
                Bytes::from_static(b"hello world"),
            ))
            .unwrap();
        // 2
        dst.as_mut()
            .start_send(Message::new(
                Reliability::Unreliable, // adjust to reliable
                0,
                Bytes::from_static(b"hello world, hello world, hello world, hello world"),
            ))
            .unwrap();
        // 1
        dst.as_mut()
            .start_send(Message::new(
                Reliability::Unreliable,
                0,
                Bytes::from_static(b"hello world"),
            ))
            .unwrap();

        assert_eq!(dst.order_write_index[0].to_u32(), 1); // 1 message on channel 0 requires ordering, next ordered frame index is 1
        assert_eq!(dst.order_write_index[1].to_u32(), 1); // 1 message on channel 1 requires ordering, next ordered frame index is 1
        assert_eq!(dst.reliable_write_index.to_u32(), 6); // 5 reliable frames, next reliable frame index is 6

        assert_eq!(dst.frame.buf.len(), 7);
        // adjusted
        assert_eq!(dst.frame.buf[4].flags.reliability, Reliability::Reliable);
        assert_eq!(dst.frame.buf[5].flags.reliability, Reliability::Reliable);
    }

    #[test]
    #[should_panic]
    fn test_fragmented_panic() {
        let dst = DstSink::default().fragmented(50, 8);
        tokio::pin!(dst);
        dst.as_mut()
            .start_send(Message::new(
                Reliability::ReliableOrdered,
                100,
                Bytes::from_static(b"hello world"),
            ))
            .unwrap();
    }

    #[test]
    fn test_fragmented_fulfill_one_packet() {
        let dst = DstSink::default().fragmented(50, 8);
        tokio::pin!(dst);
        dst.as_mut()
            .start_send(Message::new(
                Reliability::ReliableOrdered,
                0,
                Bytes::from_iter(std::iter::repeat(0xfe).take(50 - FRAME_SET_HEADER_SIZE - 10)),
            ))
            .unwrap();
        assert_eq!(dst.frame.buf.len(), 1); // 1 frame
        assert!(dst.frame.buf[0].fragment.is_none()); // not fragmented
        assert_eq!(dst.frame.buf[0].size(), 50 - FRAME_SET_HEADER_SIZE); // full size
    }

    #[test]
    fn test_fragmented_split_packet() {
        let dst = DstSink::default().fragmented(50, 8);
        tokio::pin!(dst);
        dst.as_mut()
            .start_send(Message::new(
                Reliability::ReliableOrdered,
                0,
                Bytes::from_iter(std::iter::repeat(0xfe).take(50)),
            ))
            .unwrap();
        assert_eq!(dst.frame.buf.len(), 2);
        let mut fragment = dst.frame.buf[0].fragment.unwrap();
        let r = dst.frame.buf[0].flags.reliability.size();
        assert_eq!(fragment.parted_size, 2);
        assert_eq!(fragment.parted_id, 0);
        assert_eq!(fragment.parted_index, 0);
        assert_eq!(dst.frame.buf[0].size(), 50 - FRAME_SET_HEADER_SIZE);
        assert_eq!(
            dst.frame.buf[0].body.len(),
            50 - FRAME_SET_HEADER_SIZE - r - FRAGMENT_PART_SIZE
        );

        fragment = dst.frame.buf[1].fragment.unwrap();
        assert_eq!(fragment.parted_size, 2);
        assert_eq!(fragment.parted_id, 0);
        assert_eq!(fragment.parted_index, 1);
    }

    #[test]
    fn test_fragmented_adjust_not_exceed() {
        let dst = DstSink::default().fragmented(50, 8);
        tokio::pin!(dst);
        dst.as_mut()
            .start_send(Message::new(
                Reliability::Unreliable,
                0,
                Bytes::from_iter(std::iter::repeat(0xfe).take(50)),
            ))
            .unwrap();
        assert_eq!(dst.frame.buf.len(), 2); // 2 frames
        assert_eq!(dst.frame.buf[0].flags.reliability, Reliability::Reliable); // adjusted
        assert_eq!(dst.frame.buf[1].flags.reliability, Reliability::Reliable); // adjusted

        // after adjusting reliability, the size does not exceed the MTU
        assert_eq!(dst.frame.buf[0].size(), 50 - FRAME_SET_HEADER_SIZE);
    }
}
