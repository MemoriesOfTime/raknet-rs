use std::io;
use std::sync::Arc;

use bytes::Bytes;
use futures::{Sink, Stream};
use tokio::net::UdpSocket as TokioUdpSocket;

use super::{Config, Incoming, MakeIncoming};
use crate::opts::{ConnectionInfo, TraceInfo};
use crate::Message;

impl MakeIncoming for TokioUdpSocket {
    fn make_incoming(
        self,
        config: Config,
    ) -> impl Stream<
        Item = (
            impl Stream<Item = Bytes> + TraceInfo,
            impl Sink<Message, Error = io::Error> + ConnectionInfo,
        ),
    > {
        let socket = Arc::new(self);
        Incoming::new(socket, config)
    }
}

impl MakeIncoming for Arc<TokioUdpSocket> {
    fn make_incoming(
        self,
        config: Config,
    ) -> impl Stream<
        Item = (
            impl Stream<Item = Bytes> + TraceInfo,
            impl Sink<Message, Error = io::Error> + ConnectionInfo,
        ),
    > {
        Incoming::new(self, config)
    }
}
