use std::io;
use std::net::ToSocketAddrs;
use std::sync::Arc;

use futures::StreamExt;
use minitrace::Span;
use tokio::net::UdpSocket as TokioUdpSocket;

use super::ConnectTo;
use crate::client::handler::offline::OfflineHandler;
use crate::client::handler::online::HandleOnline;
use crate::codec::frame::Framed;
use crate::codec::{Decoded, Encoded};
use crate::errors::Error;
use crate::guard::HandleOutgoing;
use crate::io::{Ping, SeparatedIO, IO};
use crate::link::{Router, TransferLink};
use crate::state::{IncomingStateManage, OutgoingStateManage};
use crate::utils::TraceStreamExt;
use crate::PeerContext;

impl ConnectTo for TokioUdpSocket {
    async fn connect_to(
        self,
        addrs: impl ToSocketAddrs,
        config: super::Config,
    ) -> Result<impl IO + Ping, Error> {
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

        let mut incoming = OfflineHandler::new(
            Framed::new(Arc::clone(&socket), config.mtu as usize), // TODO: discover MTU
            addr,
            config.offline_config(),
        )
        .await?;

        let link = TransferLink::new_arc(config.client_role());
        let dst = Framed::new(Arc::clone(&socket), config.mtu as usize)
            .handle_outgoing(
                Arc::clone(&link),
                config.send_buf_cap,
                PeerContext {
                    addr,
                    mtu: config.mtu,
                },
                config.client_role(),
            )
            .frame_encoded(config.mtu, config.codec_config(), Arc::clone(&link))
            .manage_outgoing_state(None);

        let (mut router, route) = Router::new(Arc::clone(&link));

        tokio::spawn(async move {
            while let Some(pack) = incoming.next().await {
                router.deliver(pack);
            }
        });

        let src = route
            .frame_decoded(
                config.codec_config(),
                Arc::clone(&link),
                config.client_role(),
            )
            .manage_incoming_state()
            .handle_online(addr, config.client_guid, Arc::clone(&link))
            .enter_on_item(Span::noop);

        Ok(SeparatedIO::new(src, dst))
    }
}
