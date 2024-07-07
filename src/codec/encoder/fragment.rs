use std::cmp::min;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::{Buf, Bytes};
use futures::Sink;
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{self, Flags, Frame, Ordered, Reliability};
use crate::packet::{PackType, FRAGMENT_PART_SIZE, FRAME_SET_HEADER_SIZE};
use crate::utils::u24;
use crate::Message;

pin_project! {
    pub(crate) struct Fragment<F> {
        #[pin]
        frame: F,
        mtu: u16,
        reliable_write_index: u24,
        order_write_index: Vec<u24>,
        parted_id_write: u16,
    }
}

pub(crate) trait Fragmented: Sized {
    fn fragmented(self, mtu: u16, max_channels: usize) -> Fragment<Self>;
}

impl<F> Fragmented for F
where
    F: Sink<Frame, Error = CodecError>,
{
    fn fragmented(self, mtu: u16, max_channels: usize) -> Fragment<Self> {
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
    F: Sink<Frame, Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, msg: Message) -> Result<(), Self::Error> {
        let mut this = self.project();
        let mut reliability = msg.get_reliability();
        let order_channel = msg.get_order_channel();
        let mut body = msg.into_data();

        // max_len is the maximum size of the frame body (excluding the fragment part option)
        let max_len = *this.mtu as usize - FRAME_SET_HEADER_SIZE - reliability.size();

        if body.len() > max_len {
            // adjust reliability when packet needs splitting
            reliability = match reliability {
                Reliability::Unreliable => Reliability::Reliable,
                Reliability::UnreliableSequenced => Reliability::ReliableSequenced,
                Reliability::UnreliableWithAckReceipt => Reliability::ReliableWithAckReceipt,
                _ => reliability,
            };
        }

        // get reliable_frame_index and ordered part
        let mut common = || {
            let mut reliable_frame_index = None;
            let mut ordered = None;
            if reliability.is_reliable() {
                reliable_frame_index = Some(*this.reliable_write_index);
                *this.reliable_write_index += 1;
            }
            // TODO: sequencing

            if reliability.is_sequenced_or_ordered() {
                if order_channel as usize >= this.order_write_index.len() {
                    return Err(
                        CodecError::OrderedFrame(
                            format!(
                                "sink a message with too large order channel {order_channel}, max channels {}",
                                this.order_write_index.len()
                            )
                        )
                    );
                }
                ordered = Some(Ordered {
                    frame_index: this.order_write_index[order_channel as usize],
                    channel: order_channel,
                });
            }
            Ok((reliable_frame_index, ordered))
        };

        if body.len() <= max_len {
            // not exceeding the mtu, no need to split.
            let (reliable_frame_index, ordered) = common()?;
            if reliability.is_sequenced_or_ordered() {
                this.order_write_index[order_channel as usize] += 1;
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

        // exceeding the mtu, split the data
        for parted_index in 0..parted_size {
            let (reliable_frame_index, ordered) = common()?;
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
            // FIXME: poll_ready is not ensured before start_send
            this.frame.as_mut().start_send(frame)?;
        }

        if reliability.is_sequenced_or_ordered() {
            this.order_write_index[order_channel as usize] += 1;
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

    // FIXME: wrong implementation, should be placed in other place
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();
        let mut frame = this.frame.as_mut();
        ready!(frame.as_mut().poll_ready(cx))?;

        let reliable_frame_index = Some(*this.reliable_write_index);
        *this.reliable_write_index += 1;

        // send `DisconnectNotification`
        frame.as_mut().start_send(Frame {
            flags: Flags::new(Reliability::Reliable, false),
            reliable_frame_index,
            seq_frame_index: None,
            ordered: None,
            fragment: None,
            body: Bytes::from_static(&[PackType::DisconnectNotification as u8]),
        })?;

        ready!(frame.as_mut().poll_flush(cx))?;
        frame.as_mut().poll_close(cx)
    }
}

#[cfg(test)]
mod test {
    use connected::Frames;
    use futures::SinkExt;

    use super::*;

    #[derive(Debug, Default)]
    struct DstSink {
        buf: Frames,
    }

    impl Sink<Frame> for DstSink {
        type Error = CodecError;

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

    #[tokio::test]
    async fn test_fragmented_works() {
        let mut dst = DstSink::default().fragmented(50, 8);
        // 1
        dst.send(Message::new(
            Reliability::ReliableOrdered,
            0,
            Bytes::from_static(b"hello world"),
        ))
        .await
        .unwrap();
        // 2
        dst.send(Message::new(
            Reliability::ReliableOrdered,
            1,
            Bytes::from_static(b"hello world, hello world, hello world, hello world"),
        ))
        .await
        .unwrap();
        // error
        let err = dst
            .send(Message::new(
                Reliability::ReliableOrdered,
                100,
                Bytes::from_static(b"hello world"),
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, CodecError::OrderedFrame(_)));
        // 1
        dst.send(Message::new(
            Reliability::Reliable,
            100,
            Bytes::from_static(b"hello world"),
        ))
        .await
        .unwrap();
        // 2
        dst.send(Message::new(
            Reliability::Unreliable,
            0,
            Bytes::from_static(b"hello world, hello world, hello world, hello world"),
        ))
        .await
        .unwrap();
        // 1
        dst.close().await.unwrap();

        assert_eq!(dst.order_write_index[0].to_u32(), 1);
        assert_eq!(dst.order_write_index[1].to_u32(), 1);
        assert_eq!(dst.reliable_write_index.to_u32(), 8);

        assert_eq!(dst.frame.buf.len(), 7);
        // adjusted
        assert_eq!(dst.frame.buf[4].flags.reliability, Reliability::Reliable);
        assert_eq!(dst.frame.buf[5].flags.reliability, Reliability::Reliable);

        // closed
        assert_eq!(
            dst.frame.buf[6].body,
            Bytes::from_static(&[PackType::DisconnectNotification as u8])
        );
    }

    #[tokio::test]
    async fn test_fragmented_fulfill_one_packet() {
        let mut dst = DstSink::default().fragmented(50, 8);
        dst.send(Message::new(
            Reliability::ReliableOrdered,
            0,
            Bytes::from_iter(std::iter::repeat(0xfe).take(50 - FRAME_SET_HEADER_SIZE - 10)),
        ))
        .await
        .unwrap();
        assert_eq!(dst.frame.buf.len(), 1);
        assert!(dst.frame.buf[0].fragment.is_none());
        assert_eq!(dst.frame.buf[0].size(), 50 - FRAME_SET_HEADER_SIZE);
    }

    #[tokio::test]
    async fn test_fragmented_split_packet() {
        let mut dst = DstSink::default().fragmented(50, 8);
        dst.send(Message::new(
            Reliability::ReliableOrdered,
            0,
            Bytes::from_iter(std::iter::repeat(0xfe).take(50)),
        ))
        .await
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
}
