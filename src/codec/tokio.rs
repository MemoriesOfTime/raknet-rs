use bytes::{Buf, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::errors::CodecError;
use crate::packet::connected::{Frames, FramesMut};
use crate::packet::Packet;

/// The raknet codec
pub(crate) struct Codec;

impl<B: Buf> Encoder<Packet<Frames<B>>> for Codec {
    type Error = CodecError;

    fn encode(&mut self, item: Packet<Frames<B>>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        item.write(dst);
        Ok(())
    }
}

impl Decoder for Codec {
    type Error = CodecError;
    // we might want to update the package during codec
    type Item = Packet<FramesMut>;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        Packet::read(src)
    }
}
