use std::{collections::HashMap, io::Cursor, net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use async_trait::async_trait;
use rustls::server::Accepted;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

pub struct ParsedHello {
    conn: TcpStream,
    accepted: Accepted,
    buffered: Vec<u8>,
}

impl ParsedHello {
    pub fn client_hello(&self) -> rustls::server::ClientHello<'_> {
        self.accepted.client_hello()
    }

    pub fn unread_conn(self) -> impl AsyncRead + AsyncWrite {
        let (read, write) = tokio::io::split(self.conn);
        let chained = AsyncReadExt::chain(Cursor::new(self.buffered), read);
        let joined = tokio::io::join(chained, write);
        joined
    }
}

async fn parse_hello(mut conn: TcpStream) -> Result<ParsedHello> {
    let mut buffer = Vec::with_capacity(1024);

    loop {
        conn.read_buf(&mut buffer).await?;
        let mut acceptor = rustls::server::Acceptor::default();
        acceptor.read_tls(&mut Cursor::new(&buffer))?;

        match acceptor.accept() {
            Ok(None) => {}
            Ok(Some(accepted)) => {
                return Ok(ParsedHello {
                    conn: conn,
                    accepted,
                    buffered: buffer,
                });
            }
            Err((err, mut alert)) => {
                buffer.clear();
                alert.write_all(&mut Cursor::new(&mut buffer))?;
                conn.write_all(&buffer).await?;
                return Err(anyhow::anyhow!(err));
            }
        }
    }
}

#[async_trait]
pub trait ParsedHelloConnHandler {
    async fn handle(&self, conn: ParsedHello, addr: SocketAddr) -> Result<()>;
}

pub async fn handle_conn(
    tcp_conn: TcpStream,
    addr: SocketAddr,
    handler: impl ParsedHelloConnHandler,
) -> Result<()> {
    let hello = timeout(Duration::from_secs(1), parse_hello(tcp_conn)).await??;

    // TODO: fallback?
    handler.handle(hello, addr).await
}

pub async fn handle_listener(
    tcp_listener: TcpListener,
    handler: impl ParsedHelloConnHandler + Send + Sync + Clone + 'static,
) -> Result<()> {
    loop {
        let handler = handler.clone();
        let (tcp_conn, addr) = tcp_listener.accept().await?;
        tokio::spawn(async move {
            // TODO: log
            let _ = handle_conn(tcp_conn, addr, handler).await;
        });
    }
}

pub struct SniRouter {
    pub handlers: HashMap<String, Box<dyn ParsedHelloConnHandler + Send + Sync>>,
    pub default: Option<Box<dyn ParsedHelloConnHandler + Send + Sync>>,
}

#[async_trait]
impl ParsedHelloConnHandler for Arc<SniRouter> {
    async fn handle(&self, hello: ParsedHello, addr: SocketAddr) -> Result<()> {
        let handler = self
            .handlers
            .get(
                hello
                    .client_hello()
                    .server_name()
                    .context("missing hello")?,
            )
            .or(self.default.as_ref())
            .context("no handler")?;

        handler.handle(hello, addr).await
    }
}
