use std::collections::HashMap;
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use fastrace::collector::SpanContext;
use fastrace::Span;
use futures::{ready, Sink, Stream};
use log::{debug, error, trace, warn};
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{self, FramesMut};
use crate::packet::{unconnected, Packet};
use crate::{PeerContext, RoleContext};

#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) sever_guid: u64,
    pub(crate) advertisement: Bytes,
    pub(crate) min_mtu: u16,
    pub(crate) max_mtu: u16,
    // Supported raknet versions, sorted
    pub(crate) support_version: Vec<u8>,
    pub(crate) max_pending: usize,
}

enum OfflineState {
    Listening,
    SendingPrepare(Option<(unconnected::Packet, SocketAddr)>),
    SendingFlush,
}

pin_project! {
    /// OfflineHandler takes the codec frame and perform offline handshake.
    pub(crate) struct OfflineHandler<F> {
        #[pin]
        frame: F,
        config: Config,
        // Half-connected queue
        pending: lru::LruCache<SocketAddr, u8>,
        connected: HashMap<SocketAddr, PeerContext>,
        state: OfflineState,
        role: RoleContext,
        read_span: Option<Span>,
    }
}

impl<F> OfflineHandler<F>
where
    F: Stream<Item = (Packet<FramesMut>, SocketAddr)>
        + Sink<(unconnected::Packet, SocketAddr), Error = CodecError>,
{
    pub(crate) fn new(frame: F, config: Config) -> Self {
        Self {
            frame,
            pending: lru::LruCache::new(
                NonZeroUsize::new(config.max_pending).expect("max_pending > 0"),
            ),
            role: RoleContext::Server {
                guid: config.sever_guid,
            },
            config,
            connected: HashMap::new(),
            state: OfflineState::Listening,
            read_span: None,
        }
    }

    pub(crate) fn disconnect(self: Pin<&mut Self>, addr: &SocketAddr) {
        let this = self.project();
        this.pending.pop(addr);
        this.connected.remove(addr);
    }

    fn make_incompatible_version(config: &Config) -> unconnected::Packet {
        unconnected::Packet::IncompatibleProtocol {
            server_protocol: *config.support_version.last().unwrap(),
            magic: (),
            server_guid: config.sever_guid,
        }
    }

    fn make_already_connected(config: &Config) -> unconnected::Packet {
        unconnected::Packet::AlreadyConnected {
            magic: (),
            server_guid: config.sever_guid,
        }
    }

    fn make_connection_request_failed(config: &Config) -> unconnected::Packet {
        unconnected::Packet::ConnectionRequestFailed {
            magic: (),
            server_guid: config.sever_guid,
        }
    }
}

impl<F> Stream for OfflineHandler<F>
where
    F: Stream<Item = (Packet<FramesMut>, SocketAddr)>
        + Sink<(unconnected::Packet, SocketAddr), Error = CodecError>,
{
    type Item = (connected::Packet<FramesMut>, PeerContext);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            match this.state {
                OfflineState::Listening => {}
                OfflineState::SendingPrepare(pack) => {
                    if let Err(err) = ready!(this.frame.as_mut().poll_ready(cx)) {
                        error!("[{}] send error: {err}", this.role);
                        *this.state = OfflineState::Listening;
                        continue;
                    }
                    if let Err(err) = this.frame.as_mut().start_send(pack.take().unwrap()) {
                        error!("[{}] send error: {err}", this.role);
                        *this.state = OfflineState::Listening;
                        continue;
                    }
                    *this.state = OfflineState::SendingFlush;
                    continue;
                }
                OfflineState::SendingFlush => {
                    if let Err(err) = ready!(this.frame.as_mut().poll_flush(cx)) {
                        error!("[{}] send error: {err}", this.role);
                    }
                    *this.state = OfflineState::Listening;
                }
            }

            let guard = this
                .read_span
                .get_or_insert_with(|| {
                    Span::root("offline", SpanContext::random()).with_properties(|| {
                        [
                            ("role", this.role.to_string()),
                            ("pending_size", this.pending.len().to_string()),
                            ("connected_size", this.connected.len().to_string()),
                        ]
                    })
                })
                .set_local_parent();
            let Some((packet, addr)) = ready!(this.frame.as_mut().poll_next(cx)) else {
                return Poll::Ready(None);
            };

            let pack = match packet {
                Packet::Unconnected(pack) => pack,
                Packet::Connected(pack) => {
                    if let Some(peer) = this.connected.get(&addr) {
                        drop(guard);
                        this.read_span.take();
                        return Poll::Ready(Some((pack, peer.clone())));
                    }
                    debug!(
                        "[{}] ignore packet {:?} from unconnected client {addr}",
                        this.role,
                        pack.pack_type()
                    );
                    *this.state = OfflineState::SendingPrepare(Some((
                        Self::make_connection_request_failed(this.config),
                        addr,
                    )));
                    continue;
                }
            };
            let resp = match pack {
                unconnected::Packet::UnconnectedPing { send_timestamp, .. } => {
                    unconnected::Packet::UnconnectedPong {
                        send_timestamp,
                        server_guid: this.config.sever_guid,
                        magic: (),
                        data: this.config.advertisement.clone(),
                    }
                }
                unconnected::Packet::OpenConnectionRequest1 {
                    protocol_version,
                    mtu,
                    ..
                } => {
                    if this
                        .config
                        .support_version
                        .binary_search(&protocol_version)
                        .is_err()
                    {
                        debug!(
                            "[{}] received incompatible version({protocol_version}) from {addr}",
                            this.role
                        );
                        *this.state = OfflineState::SendingPrepare(Some((
                            Self::make_incompatible_version(this.config),
                            addr,
                        )));
                        continue;
                    }
                    if this.pending.put(addr, protocol_version).is_some() {
                        debug!(
                            "[{}] received duplicate open connection request 1 from {addr}",
                            this.role
                        );
                    } else {
                        trace!(
                            "[{}] received open connection request 1 from {addr}",
                            this.role,
                        );
                    }
                    // max_mtu >= final_mtu >= min_mtu
                    let final_mtu = mtu.clamp(this.config.min_mtu, this.config.max_mtu);
                    unconnected::Packet::OpenConnectionReply1 {
                        magic: (),
                        server_guid: this.config.sever_guid,
                        use_encryption: false, // must set to false first
                        mtu: final_mtu,
                    }
                }
                unconnected::Packet::OpenConnectionRequest2 { mtu, .. } => {
                    if this.pending.pop(&addr).is_none() {
                        debug!("[{}] received open connection request 2 from {addr} without open connection request 1", this.role);
                        *this.state = OfflineState::SendingPrepare(Some((
                            Self::make_incompatible_version(this.config),
                            addr,
                        )));
                        continue;
                    }
                    trace!(
                        "[{}] received open connection request 2 from {addr}",
                        this.role
                    );
                    // client should adjust the mtu
                    if mtu < this.config.min_mtu
                        || mtu > this.config.max_mtu
                        || this.connected.contains_key(&addr)
                    {
                        debug!("[{}] received unexpected mtu({mtu}) from {addr}", this.role);
                        *this.state = OfflineState::SendingPrepare(Some((
                            Self::make_already_connected(this.config),
                            addr,
                        )));
                        continue;
                    }
                    debug!("[{}] client {addr} connected with mtu {mtu}", this.role);
                    this.connected.insert(addr, PeerContext { addr, mtu });
                    unconnected::Packet::OpenConnectionReply2 {
                        magic: (),
                        server_guid: this.config.sever_guid,
                        client_address: addr,
                        mtu,
                        encryption_enabled: false, // must set to false
                    }
                }
                _ => {
                    warn!(
                        "[{}] received a package({:?}) that should not be received on the server.",
                        this.role,
                        pack.pack_type()
                    );
                    continue;
                }
            };
            *this.state = OfflineState::SendingPrepare(Some((resp, addr)));
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::VecDeque;

    use connected::{FrameSet, Frames};
    use futures::StreamExt;

    use super::*;
    use crate::utils::tests::test_trace_log_setup;

    struct TestCase {
        addr: SocketAddr,
        source: VecDeque<Packet<FramesMut>>,
        dst: Vec<unconnected::Packet>,
    }

    impl Stream for TestCase {
        type Item = (Packet<FramesMut>, SocketAddr);

        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            if let Some(pack) = self.source.pop_front() {
                return Poll::Ready(Some((pack, self.addr)));
            }
            Poll::Ready(None)
        }
    }

    impl Sink<(unconnected::Packet, SocketAddr)> for TestCase {
        type Error = CodecError;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(
            mut self: Pin<&mut Self>,
            item: (unconnected::Packet, SocketAddr),
        ) -> Result<(), Self::Error> {
            self.dst.push(item.0);
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
    async fn test_offline_handshake_works() {
        let _guard = test_trace_log_setup();

        let client_addr = "0.0.0.1:1".parse().unwrap();
        let test_case = TestCase {
            addr: client_addr,
            source: vec![
                unconnected::Packet::UnconnectedPing {
                    send_timestamp: 0,
                    magic: (),
                    client_guid: 114514,
                },
                unconnected::Packet::OpenConnectionRequest1 {
                    magic: (),
                    protocol_version: 11,
                    mtu: 1000,
                },
                unconnected::Packet::OpenConnectionRequest2 {
                    magic: (),
                    server_address: "0.0.0.0:1".parse().unwrap(),
                    mtu: 1000,
                    client_guid: 114514,
                },
            ]
            .into_iter()
            .map(Packet::Unconnected)
            .chain(std::iter::once(Packet::Connected(
                connected::Packet::FrameSet(FrameSet {
                    seq_num: 0.into(),
                    set: Frames::new(),
                }),
            )))
            .collect(),
            dst: vec![],
        };

        let handler = OfflineHandler::new(
            test_case,
            Config {
                sever_guid: 1919810,
                advertisement: Bytes::from_static(b"hello"),
                min_mtu: 800,
                max_mtu: 1400,
                support_version: vec![8, 11, 12],
                max_pending: 10,
            },
        );
        tokio::pin!(handler);
        assert_eq!(
            handler.next().await.unwrap().0,
            connected::Packet::FrameSet(FrameSet {
                seq_num: 0.into(),
                set: vec![]
            })
        );
        assert_eq!(
            handler.project().frame.dst,
            vec![
                unconnected::Packet::UnconnectedPong {
                    send_timestamp: 0,
                    server_guid: 1919810,
                    magic: (),
                    data: Bytes::from_static(b"hello")
                },
                unconnected::Packet::OpenConnectionReply1 {
                    magic: (),
                    server_guid: 1919810,
                    use_encryption: false,
                    mtu: 1000
                },
                unconnected::Packet::OpenConnectionReply2 {
                    magic: (),
                    server_guid: 1919810,
                    client_address: client_addr,
                    mtu: 1000,
                    encryption_enabled: false
                },
            ]
        );
    }

    #[tokio::test]
    async fn test_offline_reject_unconnected_packet() {
        let _guard = test_trace_log_setup();

        let test_case = TestCase {
            addr: "0.0.0.2:1".parse().unwrap(),
            source: vec![Packet::Connected(connected::Packet::FrameSet(FrameSet {
                seq_num: 0.into(),
                set: Frames::new(),
            }))]
            .into_iter()
            .collect(),
            dst: vec![],
        };
        let handler = OfflineHandler::new(
            test_case,
            Config {
                sever_guid: 1919810,
                advertisement: Bytes::from_static(b"hello"),
                min_mtu: 800,
                max_mtu: 1400,
                support_version: vec![8, 11, 12],
                max_pending: 10,
            },
        );
        tokio::pin!(handler);
        assert!(handler.next().await.is_none());
        assert_eq!(
            handler.project().frame.dst,
            vec![unconnected::Packet::ConnectionRequestFailed {
                magic: (),
                server_guid: 1919810,
            }]
        );
    }

    #[tokio::test]
    async fn test_offline_reject_wrong_connect_flow() {
        let _guard = test_trace_log_setup();

        let test_cases = [
            (
                TestCase {
                    addr: "0.0.0.3:1".parse().unwrap(),
                    // wrong order
                    source: vec![
                        unconnected::Packet::OpenConnectionRequest2 {
                            magic: (),
                            server_address: "0.0.0.0:1".parse().unwrap(),
                            mtu: 1000,
                            client_guid: 114514,
                        },
                        unconnected::Packet::OpenConnectionRequest1 {
                            magic: (),
                            protocol_version: 11,
                            mtu: 1000,
                        },
                    ]
                    .into_iter()
                    .map(Packet::Unconnected)
                    .collect(),
                    dst: vec![],
                },
                vec![
                    unconnected::Packet::IncompatibleProtocol {
                        server_protocol: 12,
                        magic: (),
                        server_guid: 1919810,
                    },
                    unconnected::Packet::OpenConnectionReply1 {
                        magic: (),
                        server_guid: 1919810,
                        use_encryption: false,
                        mtu: 1000,
                    },
                ],
            ),
            (
                TestCase {
                    addr: "0.0.0.4:1".parse().unwrap(),
                    // wrong protocol version
                    source: vec![unconnected::Packet::OpenConnectionRequest1 {
                        magic: (),
                        protocol_version: 100,
                        mtu: 1000,
                    }]
                    .into_iter()
                    .map(Packet::Unconnected)
                    .collect(),
                    dst: vec![],
                },
                vec![unconnected::Packet::IncompatibleProtocol {
                    server_protocol: 12,
                    magic: (),
                    server_guid: 1919810,
                }],
            ),
            (
                TestCase {
                    addr: "0.0.0.5:1".parse().unwrap(),
                    // already connected
                    source: vec![
                        unconnected::Packet::OpenConnectionRequest1 {
                            magic: (),
                            protocol_version: 11,
                            mtu: 1000,
                        },
                        unconnected::Packet::OpenConnectionRequest2 {
                            magic: (),
                            server_address: "0.0.0.0:1".parse().unwrap(),
                            mtu: 1000,
                            client_guid: 114514,
                        },
                        unconnected::Packet::OpenConnectionRequest1 {
                            magic: (),
                            protocol_version: 11,
                            mtu: 1000,
                        },
                        unconnected::Packet::OpenConnectionRequest2 {
                            magic: (),
                            server_address: "0.0.0.0:1".parse().unwrap(),
                            mtu: 1000,
                            client_guid: 114514,
                        },
                    ]
                    .into_iter()
                    .map(Packet::Unconnected)
                    .collect(),
                    dst: vec![],
                },
                vec![
                    unconnected::Packet::OpenConnectionReply1 {
                        magic: (),
                        server_guid: 1919810,
                        use_encryption: false,
                        mtu: 1000,
                    },
                    unconnected::Packet::OpenConnectionReply2 {
                        magic: (),
                        server_guid: 1919810,
                        client_address: "0.0.0.5:1".parse().unwrap(),
                        mtu: 1000,
                        encryption_enabled: false, // must set to false
                    },
                    unconnected::Packet::OpenConnectionReply1 {
                        magic: (),
                        server_guid: 1919810,
                        use_encryption: false,
                        mtu: 1000,
                    },
                    unconnected::Packet::AlreadyConnected {
                        magic: (),
                        server_guid: 1919810,
                    },
                ],
            ),
            (
                TestCase {
                    addr: "0.0.0.6:1".parse().unwrap(),
                    // disjoint mtu
                    source: vec![
                        unconnected::Packet::OpenConnectionRequest1 {
                            magic: (),
                            protocol_version: 11,
                            mtu: 10,
                        },
                        unconnected::Packet::OpenConnectionRequest2 {
                            magic: (),
                            server_address: "0.0.0.0:1".parse().unwrap(),
                            mtu: 10,
                            client_guid: 114514,
                        },
                    ]
                    .into_iter()
                    .map(Packet::Unconnected)
                    .collect(),
                    dst: vec![],
                },
                vec![
                    unconnected::Packet::OpenConnectionReply1 {
                        magic: (),
                        server_guid: 1919810,
                        use_encryption: false,
                        mtu: 800,
                    },
                    unconnected::Packet::AlreadyConnected {
                        magic: (),
                        server_guid: 1919810,
                    },
                ],
            ),
        ];

        for (case, expect) in test_cases {
            let handler = OfflineHandler::new(
                case,
                Config {
                    sever_guid: 1919810,
                    advertisement: Bytes::from_static(b"hello"),
                    min_mtu: 800,
                    max_mtu: 1400,
                    support_version: vec![8, 11, 12],
                    max_pending: 10,
                },
            );
            tokio::pin!(handler);
            assert!(handler.next().await.is_none());
            assert_eq!(handler.project().frame.dst, expect);
        }
    }
}
