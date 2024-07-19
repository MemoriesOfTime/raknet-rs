use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::Bytes;
use futures::Stream;
use log::debug;
use pin_project_lite::pin_project;

use crate::link::SharedLink;
use crate::packet::connected::FrameBody;
use crate::utils::timestamp;
use crate::RoleContext;

pub(crate) trait HandleOnline: Sized {
    fn handle_online(
        self,
        addr: SocketAddr,
        client_guid: u64,
        link: SharedLink,
    ) -> OnlineHandler<Self>;
}

impl<F> HandleOnline for F
where
    F: Stream<Item = FrameBody>,
{
    fn handle_online(
        self,
        addr: SocketAddr,
        client_guid: u64,
        link: SharedLink,
    ) -> OnlineHandler<Self> {
        link.send_frame_body(FrameBody::ConnectionRequest {
            client_guid,
            request_timestamp: timestamp(),
            use_encryption: false,
        });
        OnlineHandler {
            frame: self,
            state: State::WaitConnRes,
            addr,
            link,
            role: RoleContext::Client { guid: client_guid },
        }
    }
}

pin_project! {
    pub(crate) struct OnlineHandler<F> {
        #[pin]
        frame: F,
        state: State,
        addr: SocketAddr,
        link: SharedLink,
        role: RoleContext,
    }
}

enum State {
    WaitConnRes,
    Connected,
}

impl<F> Stream for OnlineHandler<F>
where
    F: Stream<Item = FrameBody>,
{
    type Item = Bytes;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            match this.state {
                State::WaitConnRes => {
                    let Some(body) = ready!(this.frame.as_mut().poll_next(cx)) else {
                        return Poll::Ready(None);
                    };
                    if let FrameBody::ConnectionRequestAccepted {
                        system_addresses,
                        accepted_timestamp,
                        ..
                    } = body
                    {
                        this.link.send_frame_body(FrameBody::NewIncomingConnection {
                            server_address: *this.addr,
                            system_addresses,
                            request_timestamp: timestamp(),
                            accepted_timestamp,
                        });
                        *this.state = State::Connected;
                        debug!(
                            "[{}] connected to server {addr:?}",
                            this.role,
                            addr = this.addr
                        );
                        continue;
                    }
                    debug!("[{}] ignore packet {body:?} on WaitConnRes", this.role);
                }
                State::Connected => {
                    let Some(body) = ready!(this.frame.as_mut().poll_next(cx)) else {
                        return Poll::Ready(None);
                    };
                    match body {
                        FrameBody::DetectLostConnections => {
                            this.link.send_frame_body(FrameBody::ConnectedPing {
                                client_timestamp: timestamp(),
                            });
                        }
                        FrameBody::User(data) => return Poll::Ready(Some(data)),
                        _ => {
                            debug!("[{}] ignore packet {body:?} on Connected", this.role);
                        }
                    }
                }
            }
        }
    }
}
