use std::{io::IoSlice, pin::Pin, task::Poll, time::Duration};

use anyhow::Result;
use tokio::{
    io::{self, AsyncRead, AsyncWrite},
    time::Instant,
};

struct TimeoutWrapper {
    timeout: Option<Pin<Box<tokio::time::Sleep>>>,
    idle_duration: Duration,
}

impl TimeoutWrapper {
    fn new(idle_duration: Duration) -> Self {
        Self {
            timeout: None,
            idle_duration: idle_duration,
        }
    }

    fn reset(&mut self, reason: &str) {
        println!("reset timeout: {}", reason);
        match &mut self.timeout {
            Some(timeout) => {
                timeout.as_mut().reset(Instant::now() + self.idle_duration);
            }
            None => {
                self.timeout = Some(Box::new(tokio::time::sleep(self.idle_duration)).into());
            }
        }
    }

    fn arm(&mut self, reason: &str) {
        if let None = self.timeout {
            println!("armed timeout: {}", reason);
            self.timeout = Some(Box::new(tokio::time::sleep(self.idle_duration)).into());
        }
    }

    fn poll<A>(
        &mut self,
        cx: &mut std::task::Context<'_>,
        reason: &str,
    ) -> Poll<Result<A, io::Error>> {
        match &mut self.timeout {
            Some(timeout) => match timeout.as_mut().poll(cx) {
                Poll::Ready(_) => {
                    println!("triggered timeout: {}", reason);
                    Poll::Ready(Err(io::ErrorKind::TimedOut.into()))
                }
                _ => Poll::Pending,
            },
            None => Poll::Pending,
        }
    }
}

pin_project_lite::pin_project! {
    pub struct TimeoutStream<A> {
        #[pin]
        inner: A,

        timeout: TimeoutWrapper,
    }
}

impl<A> TimeoutStream<A> {
    pub fn new(inner: A, idle_duration: Duration) -> Self {
        Self {
            inner: inner,
            timeout: TimeoutWrapper::new(idle_duration),
        }
    }
}

impl<A> AsyncRead for TimeoutStream<A>
where
    A: AsyncRead,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<Result<(), io::Error>> {
        let projected = self.project();
        let inner = projected.inner.poll_read(cx, buf);
        if inner.is_ready() {
            projected.timeout.reset("read");
            return inner;
        };
        projected.timeout.arm("read");
        projected.timeout.poll(cx, "read")
    }
}

impl<A> AsyncWrite for TimeoutStream<A>
where
    A: AsyncWrite,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let projected = self.project();
        let inner = projected.inner.poll_write(cx, buf);
        if inner.is_ready() {
            projected.timeout.reset("write");
            return inner;
        };
        projected.timeout.arm("write");
        projected.timeout.poll(cx, "write")
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context,
    ) -> Poll<Result<(), io::Error>> {
        let projected = self.project();
        let inner = projected.inner.poll_flush(cx);
        if inner.is_ready() {
            projected.timeout.reset("flush");
            return inner;
        };
        projected.timeout.arm("flush");
        projected.timeout.poll(cx, "flush")
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context,
    ) -> Poll<Result<(), io::Error>> {
        let projected = self.project();
        let inner = projected.inner.poll_shutdown(cx);
        if inner.is_ready() {
            projected.timeout.reset("shutdown");
            return inner;
        };
        projected.timeout.arm("shutdown");
        projected.timeout.poll(cx, "shutdown")
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let projected = self.project();
        let inner = projected.inner.poll_write_vectored(cx, bufs);
        if inner.is_ready() {
            projected.timeout.reset("write vectored");
            return inner;
        };
        projected.timeout.arm("write vectored");
        projected.timeout.poll(cx, "write vectored")
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}
