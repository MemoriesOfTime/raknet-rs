use std::future::Future;
use std::io;
use std::net::ToSocketAddrs;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use futures::{Sink, SinkExt, Stream, StreamExt};
use log::{error, trace};

use super::handler::offline;
use super::handler::online::HandleOnline;
use crate::codec::frame::Framed;
use crate::codec::{AsyncSocket, Decoded, Encoded};
use crate::guard::HandleOutgoing;
use crate::link::{Route, TransferLink};
use crate::opts::{ConnectionInfo, Ping, WrapConnectionInfo};
use crate::state::{IncomingStateManage, OutgoingStateManage};
use crate::utils::Logged;
use crate::{codec, Message, Role};

/// Connection implementation by using tokio's UDP framework
#[cfg(feature = "tokio-rt")]
mod tokio;

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Config {
    /// The given mtu, the default value is 1400
    mtu: u16,
    /// The client guid, used to identify the client, initialized by random
    client_guid: u64,
    /// Raknet protocol version, default is 9
    protocol_version: u8,
    /// Limit the max size of a parted frames set, 0 means no limit
    /// It will abort the split frame if the `parted_size` reaches limit.
    /// The maximum number of inflight parted frames is `max_parted_size` * `max_parted_count`
    max_parted_size: u32,
    /// Limit the max count of **all** parted frames sets from an address.
    /// It might cause client resending frames if the limit is reached.
    /// The maximum number of inflight parted frames is `max_parted_size` * `max_parted_count`
    max_parted_count: usize,
    /// Maximum ordered channel, the value should be less than 256
    max_channels: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

impl Config {
    pub fn new() -> Self {
        Self {
            mtu: 1400,
            client_guid: rand::random(),
            protocol_version: 9,
            max_parted_size: 256,
            max_parted_count: 256,
            max_channels: 1,
        }
    }

    /// Give the mtu of the connection
    pub fn mtu(mut self, mtu: u16) -> Self {
        self.mtu = mtu;
        self
    }

    /// Set the client guid
    pub fn client_guid(mut self, client_guid: u64) -> Self {
        self.client_guid = client_guid;
        self
    }

    /// Set the protocol version
    pub fn protocol_version(mut self, protocol_version: u8) -> Self {
        self.protocol_version = protocol_version;
        self
    }

    /// Set the maximum parted size
    /// The default value is 256
    /// The maximum number of inflight parted frames is `max_parted_size`*`max_parted_count`nt
    pub fn max_parted_size(mut self, size: u32) -> Self {
        self.max_parted_size = size;
        self
    }

    /// Set the maximum parted count
    /// The default value is 256
    /// The maximum number of inflight parted frames is `max_parted_size`*`max_parted_count`nt
    pub fn max_parted_count(mut self, count: usize) -> Self {
        self.max_parted_count = count;
        self
    }

    /// Set the maximum channels
    /// The default value is 1
    /// The maximum value should be less than 256
    /// # Panics
    /// Panics if the channels is greater than 256
    pub fn max_channels(mut self, channels: usize) -> Self {
        assert!(channels < 256, "max_channels should be less than 256");
        self.max_channels = channels;
        self
    }

    fn offline_config(&self) -> offline::Config {
        offline::Config {
            client_guid: self.client_guid,
            mtu: self.mtu,
            protocol_version: self.protocol_version,
        }
    }

    fn codec_config(&self) -> codec::Config {
        codec::Config {
            max_parted_count: self.max_parted_count,
            max_parted_size: self.max_parted_size,
            max_channels: self.max_channels,
        }
    }

    fn client_role(&self) -> Role {
        Role::Client {
            guid: self.client_guid,
        }
    }
}

pub trait ConnectTo: Sized {
    #[allow(async_fn_in_trait)] // No need to consider the auto trait for now.
    async fn connect_to(
        self,
        addrs: impl ToSocketAddrs,
        config: Config,
    ) -> io::Result<(
        impl Stream<Item = Bytes>,
        impl Sink<Message, Error = io::Error> + Ping + ConnectionInfo,
    )>;
}

pub(crate) async fn connect_to<H>(
    socket: impl AsyncSocket,
    addrs: impl ToSocketAddrs,
    config: super::Config,
    runtime: impl Fn(Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>) -> H,
) -> io::Result<(
    impl Stream<Item = Bytes>,
    impl Sink<Message, Error = io::Error> + Ping + ConnectionInfo,
)> {
    let mut lookups = addrs.to_socket_addrs()?;
    let addr = lookups
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "invalid address"))?;

    let (mut incoming, peer) = offline::OfflineHandler::new(
        Framed::new(socket.clone(), config.mtu as usize), // TODO: discover MTU
        addr,
        config.offline_config(),
    )
    .await?;
    let role = config.client_role();

    let link = TransferLink::new_arc(role, peer);
    let mut dst = Framed::new(socket.clone(), peer.mtu as usize)
        .handle_outgoing(Arc::clone(&link), peer, role)
        .frame_encoded(peer.mtu, config.codec_config(), Arc::clone(&link))
        .manage_outgoing_state(None)
        .wrap_connection_info(peer);

    let (mut router, route) = Route::new(Arc::clone(&link));

    runtime(Box::pin(async move {
        while let Some(pack) = incoming.next().await {
            // deliver the packet actively so that we do not miss ACK/NACK packets to advance
            // the outgoing state
            router.deliver(pack);
        }
    }));

    let src = route
        .frame_decoded(config.codec_config())
        .logged(
            move |frame| trace!("[{role}] received {frame:?} from {peer}"),
            move |err| error!("[{role}] decode error: {err} from {peer}"),
        )
        .manage_incoming_state()
        .handle_online(addr, config.client_guid, Arc::clone(&link));

    // make all internal packets flushed
    SinkExt::<Message>::flush(&mut dst).await?;
    Ok((src, dst))
}
