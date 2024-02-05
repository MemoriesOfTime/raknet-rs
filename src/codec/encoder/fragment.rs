use std::cmp::min;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::{Buf, Bytes};
use futures::Sink;
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{self, Flags, Frame, Frames, Ordered, Reliability, Uint24le};
use crate::packet::{PackType, FRAME_SET_HEADER_SIZE};
use crate::Message;

pin_project! {
    pub(crate) struct Fragment<F> {
        #[pin]
        frame: F,
        mtu: u16,
        reliable_write_index: u32,
        order_write_index: Vec<u32>,
        split_write_index: u16,
    }
}

pub(crate) trait Fragmented: Sized {
    fn fragmented(self, mtu: u16, max_channels: usize) -> Fragment<Self>;
}

impl<F> Fragmented for F
where
    F: Sink<Frames<Bytes>, Error = CodecError> + Sink<Frame<Bytes>, Error = CodecError>,
{
    fn fragmented(self, mtu: u16, max_channels: usize) -> Fragment<Self> {
        Fragment {
            frame: self,
            mtu,
            reliable_write_index: 0,
            order_write_index: std::iter::repeat(0).take(max_channels).collect(),
            split_write_index: 0,
        }
    }
}

impl<F> Sink<Message> for Fragment<F>
where
    F: Sink<Frames<Bytes>, Error = CodecError> + Sink<Frame<Bytes>, Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<Frames<Bytes>>::poll_ready(self.project().frame, cx)
    }

    fn start_send(self: Pin<&mut Self>, msg: Message) -> Result<(), Self::Error> {
        let this = self.project();

        let mut reliability = msg.get_reliability();
        let order_channel = msg.get_order_channel();
        let max_len = *this.mtu as usize - FRAME_SET_HEADER_SIZE - reliability.size();

        if msg.get_data().len() >= max_len {
            // adjust reliability when packet needs splitting
            reliability = match reliability {
                Reliability::Unreliable => Reliability::Reliable,
                Reliability::UnreliableSequenced => Reliability::ReliableSequenced,
                Reliability::UnreliableWithAckReceipt => Reliability::ReliableWithAckReceipt,
                _ => reliability,
            };
        }

        let mut common = || {
            let mut reliable_frame_index = None;
            let mut ordered = None;
            if reliability.is_reliable() {
                reliable_frame_index = Some(Uint24le(*this.reliable_write_index));
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
                    frame_index: Uint24le(this.order_write_index[order_channel as usize]),
                    channel: order_channel,
                });
            }
            Ok((reliable_frame_index, ordered))
        };

        if msg.get_data().len() < max_len {
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
                body: msg.into_data(),
            };
            return this.frame.start_send(frame);
        }

        // subtract the fragment part size
        let per_len = max_len - 10;

        let split = (msg.get_data().len() - 1) / per_len + 1;
        let parted_id = *this.split_write_index;
        *this.split_write_index += 1;

        // exceeding the mtu, split the data
        let mut frames = Vec::with_capacity(split);
        let mut data = msg.into_data();
        for i in 0..split {
            let (reliable_frame_index, ordered) = common()?;
            let frame = Frame {
                flags: Flags::new(reliability, true),
                reliable_frame_index,
                seq_frame_index: None,
                ordered,
                fragment: Some(connected::Fragment {
                    parted_size: split as u32,
                    parted_id,
                    parted_index: i as u32,
                }),
                body: data.split_to(min(per_len, data.len())),
            };
            frames.push(frame);
        }

        if reliability.is_sequenced_or_ordered() {
            this.order_write_index[order_channel as usize] += 1;
        }

        debug_assert!(
            data.remaining() == 0,
            "split failed, there still remains data"
        );

        this.frame.start_send(frames)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<Frames<Bytes>>::poll_flush(self.project().frame, cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();
        let mut frame = this.frame.as_mut();
        ready!(Sink::<Frames<Bytes>>::poll_ready(frame.as_mut(), cx))?;

        let reliable_frame_index = Some(Uint24le(*this.reliable_write_index));
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

        ready!(Sink::<Frames<Bytes>>::poll_flush(frame.as_mut(), cx))?;
        Sink::<Frames<Bytes>>::poll_close(frame.as_mut(), cx)
    }
}

#[cfg(test)]
mod test {
    use futures::SinkExt;

    use super::*;

    #[derive(Debug, Default)]
    struct DstSink {
        buf: Frames<Bytes>,
    }

    impl Sink<Frames<Bytes>> for DstSink {
        type Error = CodecError;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, item: Frames<Bytes>) -> Result<(), Self::Error> {
            self.buf.extend(item);
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

    impl Sink<Frame<Bytes>> for DstSink {
        type Error = CodecError;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, item: Frame<Bytes>) -> Result<(), Self::Error> {
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
        dst.send(Message::new(
            Reliability::ReliableOrdered,
            100,
            Bytes::from_static(b"hello world"),
        ))
        .await
        .unwrap_err();
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

        assert_eq!(dst.order_write_index[0], 1);
        assert_eq!(dst.order_write_index[1], 1);
        assert_eq!(dst.reliable_write_index, 8);

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
}
