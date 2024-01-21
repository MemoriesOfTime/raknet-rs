use bytes::Bytes;
use futures::{Sink, Stream};

mod handshake;
mod incoming;
mod offline;

// Provide the basic operation for each connection, produced by [`Incoming`]
type IO = impl Stream<Item = Bytes> + Sink<Bytes>;
