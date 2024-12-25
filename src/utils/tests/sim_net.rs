use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::{BufMut, Bytes, BytesMut};
use futures::{Sink, Stream};
use parking_lot::Mutex;
use rand::seq::SliceRandom;
use rand::Rng;

use crate::client::{self, ConnectTo};
use crate::codec::AsyncSocket;
use crate::opts::{ConnectionInfo, Ping, TraceInfo};
use crate::server::{Incoming, MakeIncoming};
use crate::utils::Reactor;
use crate::Message;

// packets will be received in order by default
type NetChan = VecDeque<(Vec<u8> /* data */, Instant /* receive at */)>;

pub(crate) struct SimNet {
    // fake ip, used to distinguish different sim net
    ip: IpAddr,
    // fake port
    allocated_port: u16,
    // sim net topology
    adjacency: HashMap<SocketAddr, HashSet<SocketAddr>>,
}

impl SimNet {
    pub(crate) fn bind(ip: IpAddr) -> Self {
        Self {
            ip,
            allocated_port: 0,
            adjacency: HashMap::new(),
        }
    }

    pub(crate) fn add_endpoint(&mut self) -> Endpoint {
        self.allocated_port += 1;
        Endpoint {
            addr: SocketAddr::new(self.ip, self.allocated_port),
            inboxes: HashMap::new(),
            mailboxes: HashMap::new(),
            nemeses: HashMap::new(),
        }
    }

    pub(crate) fn connect(&mut self, p1: &mut Endpoint, p2: &mut Endpoint) {
        assert_eq!(p1.addr.ip(), p2.addr.ip());
        assert_eq!(p1.addr.ip(), self.ip);

        let p1top2 = Arc::new(Mutex::new(NetChan::new()));
        let p2top1 = Arc::new(Mutex::new(NetChan::new()));

        p1.mailboxes.insert(p2.addr, p1top2.clone());
        p1.nemeses.insert(p2.addr, Nemeses::new());
        p1.inboxes.insert(p2.addr, p2top1.clone());

        p2.mailboxes.insert(p1.addr, p2top1);
        p2.nemeses.insert(p1.addr, Nemeses::new());
        p2.inboxes.insert(p1.addr, p1top2);

        self.adjacency.entry(p1.addr).or_default().insert(p2.addr);
        self.adjacency.entry(p2.addr).or_default().insert(p1.addr);
    }

    pub(crate) fn add_nemesis(&self, from: &mut Endpoint, to: &Endpoint, nemesis: impl Nemesis) {
        assert_eq!(from.addr.ip(), to.addr.ip());
        assert_eq!(from.addr.ip(), self.ip);

        from.nemeses
            .get_mut(&to.addr)
            .expect("endpoints do not have connected")
            .push(Box::new(nemesis));
    }
}

pub(crate) struct Endpoint {
    addr: SocketAddr,
    inboxes: HashMap<SocketAddr, Arc<Mutex<NetChan>>>,
    mailboxes: HashMap<SocketAddr, Arc<Mutex<NetChan>>>,
    nemeses: HashMap<SocketAddr, Nemeses>,
}

impl Endpoint {
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl MakeIncoming for Endpoint {
    fn make_incoming(
        self,
        config: crate::server::Config,
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

impl ConnectTo for Endpoint {
    async fn connect_to(
        self,
        addrs: impl ToSocketAddrs,
        config: crate::client::Config,
    ) -> io::Result<(
        impl Stream<Item = Bytes>,
        impl Sink<Message, Error = io::Error> + Ping + ConnectionInfo,
    )> {
        let socket = Arc::new(self);
        client::connect_to(socket, addrs, config, tokio::spawn).await
    }
}

impl AsyncSocket for Arc<Endpoint> {
    fn poll_recv_from(
        &self,
        cx: &mut Context<'_>,
        buf: &mut BytesMut,
    ) -> Poll<io::Result<SocketAddr>> {
        let now = Instant::now();
        let mut next_poll = now + Duration::from_millis(10);
        for (from, chan) in &self.inboxes {
            let mut chan = chan.lock();
            if chan.is_empty() {
                continue;
            }
            let receive_at = chan.front().map(|x| x.1).unwrap();
            if receive_at > now {
                // net delay
                next_poll = next_poll.min(receive_at);
                continue;
            }
            let (data, _) = chan.pop_front().unwrap();
            if data.is_empty() {
                // data loss
                continue;
            }
            buf.put(&data[..]);
            return Poll::Ready(Ok(*from));
        }
        Reactor::get().insert_timer(0, next_poll, cx.waker());
        Poll::Pending
    }

    fn poll_send_to(
        &self,
        _: &mut Context<'_>,
        buf: &[u8],
        target: SocketAddr,
    ) -> Poll<io::Result<usize>> {
        let mut chan = self.mailboxes.get(&target).expect("not connected").lock();
        let nemeses = self.nemeses.get(&target).expect("not connect");
        let now = Instant::now();
        chan.push_back((buf.to_vec(), now));
        for nemesis in nemeses {
            nemesis.strike(&mut chan);
        }
        Poll::Ready(Ok(buf.len()))
    }
}

type Nemeses = Vec<Box<dyn Nemesis>>;

pub(crate) trait Nemesis: Send + Sync + 'static {
    fn strike(&self, inflight: &mut NetChan);
}

// Make packet disorder
pub(crate) struct RandomDataDisorder {
    pub(crate) odd: f32,
    pub(crate) rate: f32,
}

impl Nemesis for RandomDataDisorder {
    fn strike(&self, inflight: &mut NetChan) {
        let mut rng = rand::thread_rng();
        if self.odd <= rng.r#gen() {
            return;
        }
        let amount = (inflight.len() as f32 * self.rate) as usize;
        let (front, back) = inflight.as_mut_slices();
        front.partial_shuffle(&mut rng, amount);
        if amount > front.len() {
            back.partial_shuffle(&mut rng, amount - front.len());
        }
    }
}

// Make packets loss
pub(crate) struct RandomDataLoss {
    pub(crate) odd: f32,
    pub(crate) rate: f32,
}

impl Nemesis for RandomDataLoss {
    fn strike(&self, inflight: &mut NetChan) {
        let mut rng = rand::thread_rng();
        if self.odd <= rng.r#gen() {
            return;
        }
        let mut amount = (inflight.len() as f32 * self.rate) as usize;
        inflight.make_contiguous();
        let (slice, _) = inflight.as_mut_slices();
        while let Some(ele) = slice.choose_mut(&mut rng) {
            if ele.0.is_empty() {
                continue;
            }
            if amount == 0 {
                return;
            }
            ele.0.clear();
            amount -= 1;
        }
    }
}

// Delay all packets in a sim net connection
pub(crate) struct RandomNetDelay {
    pub(crate) odd: f32,
    pub(crate) delay: Duration,
}

impl Nemesis for RandomNetDelay {
    fn strike(&self, inflight: &mut NetChan) {
        if self.odd <= rand::random() {
            return;
        }
        inflight[0].1 += self.delay;
    }
}
