use std::io;
use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use bytes::{BufMut, BytesMut};
use tokio::net::UdpSocket as TokioUdpSocket;

use super::AsyncSocket;

impl AsyncSocket for Arc<TokioUdpSocket> {
    fn poll_recv_from(
        &self,
        cx: &mut Context<'_>,
        rd: &mut BytesMut,
    ) -> Poll<io::Result<SocketAddr>> {
        // Safety: `chunk_mut()` returns a `&mut UninitSlice`, and `UninitSlice` is a
        // transparent wrapper around `[MaybeUninit<u8>]`.
        let buf = unsafe { &mut *(rd.chunk_mut() as *mut _ as *mut [MaybeUninit<u8>]) };
        let mut read = tokio::io::ReadBuf::uninit(buf);
        let ptr = read.filled().as_ptr();
        let res = ready!(self.as_ref().poll_recv_from(cx, &mut read));

        assert_eq!(ptr, read.filled().as_ptr());
        let addr = res?;

        // Safety: This is guaranteed to be the number of initialized (and read) bytes due
        // to the invariants provided by `ReadBuf::filled`.
        unsafe { rd.advance_mut(read.filled().len()) };

        Poll::Ready(Ok(addr))
    }

    fn poll_send_to(
        &self,
        cx: &mut Context<'_>,
        buf: &[u8],
        target: SocketAddr,
    ) -> Poll<io::Result<usize>> {
        self.as_ref().poll_send_to(cx, buf, target)
    }
}
