use std::io;
use std::net::ToSocketAddrs;
use std::sync::Arc;

use log::debug;
use minitrace::Span;
use tokio::net::UdpSocket as TokioUdpSocket;
use tokio_util::udp::UdpFramed;

use super::ConnectTo;
use crate::client::handler::offline::HandleOffline;
use crate::client::handler::online::HandleOnline;
use crate::codec::tokio::Codec;
use crate::codec::{Decoded, Encoded};
use crate::errors::Error;
use crate::guard::HandleOutgoing;
use crate::io::{SeparatedIO, IO};
use crate::link::TransferLink;
use crate::state::{IncomingStateManage, OutgoingStateManage};
use crate::utils::{Logged, TraceStreamExt};
use crate::PeerContext;

impl ConnectTo for TokioUdpSocket {
    async fn connect_to(
        self,
        addrs: impl ToSocketAddrs,
        config: super::Config,
    ) -> Result<impl IO, Error> {
        let socket = Arc::new(self);
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

        let ack = TransferLink::new_arc(config.client_role());

        let dst = UdpFramed::new(Arc::clone(&socket), Codec)
            .handle_outgoing(
                Arc::clone(&ack),
                config.send_buf_cap,
                PeerContext {
                    addr,
                    mtu: config.mtu,
                },
                config.client_role(),
            )
            .frame_encoded(config.mtu, config.codec_config(), Arc::clone(&ack))
            .manage_outgoing_state();

        let incoming = UdpFramed::new(socket, Codec)
            .logged_err(|err| {
                debug!("codec error: {err} when decode offline frames");
            })
            .handle_offline(addr, config.offline_config())
            .await?;

        let src = ack
            .filter_incoming_ack(incoming)
            .frame_decoded(
                config.codec_config(),
                Arc::clone(&ack),
                config.client_role(),
            )
            .manage_incoming_state()
            .handle_online(addr, config.client_guid, Arc::clone(&ack))
            .enter_on_item(Span::noop);

        Ok(SeparatedIO::new(src, dst))
    }
}
