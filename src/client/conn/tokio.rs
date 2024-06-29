use std::io;
use std::net::ToSocketAddrs;
use std::sync::Arc;

use log::debug;
use tokio::net::UdpSocket as TokioUdpSocket;
use tokio_util::udp::UdpFramed;

use super::ConnectTo;
use crate::client::handler::offline::HandleOffline;
use crate::client::handler::online::HandleOnline;
use crate::codec::tokio::Codec;
use crate::codec::{Decoded, Encoded};
use crate::errors::Error;
use crate::guard::{HandleIncoming, HandleOutgoingAck};
use crate::io::{IOImpl, IO};
use crate::utils::{priority_mpsc, Logged};
use crate::{PeerContext, RoleContext};

impl ConnectTo for TokioUdpSocket {
    async fn connect_to(
        self,
        addrs: impl ToSocketAddrs,
        config: super::Config,
    ) -> Result<impl IO, Error> {
        let socket = Arc::new(self);

        let (incoming_ack_tx, incoming_ack_rx) = flume::unbounded();
        let (incoming_nack_tx, incoming_nack_rx) = flume::unbounded();

        let (outgoing_ack_tx, outgoing_ack_rx) = priority_mpsc::unbounded();
        let (outgoing_nack_tx, outgoing_nack_rx) = priority_mpsc::unbounded();

        let mut lookups = addrs.to_socket_addrs()?;

        let addr = loop {
            if let Some(addr) = lookups.next() {
                if socket.connect(addr).await.is_ok() {
                    break addr;
                }
                continue;
            }
            return Err(io::Error::new(io::ErrorKind::AddrNotAvailable, "invalid address").into());
        };

        let write = UdpFramed::new(Arc::clone(&socket), Codec)
            .handle_outgoing(
                incoming_ack_rx,
                incoming_nack_rx,
                outgoing_ack_rx,
                outgoing_nack_rx,
                config.send_buf_cap,
                PeerContext {
                    addr,
                    mtu: config.mtu,
                },
                RoleContext::Client,
            )
            .frame_encoded(config.mtu, config.codec_config());

        let io = UdpFramed::new(socket, Codec)
            .logged_err(|err| {
                debug!("[client] got codec error: {err} when decode offline frames");
            })
            .handle_offline(addr, config.offline_config())
            .await?
            .handle_incoming(incoming_ack_tx, incoming_nack_tx)
            .decoded(
                config.codec_config(),
                outgoing_ack_tx,
                outgoing_nack_tx,
                RoleContext::Client,
            )
            .handle_online(write, addr, config.client_guid)
            .await?;

        Ok(IOImpl::new(io))
    }
}
