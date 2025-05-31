use std::{convert::Infallible, net::SocketAddr, sync::Arc};

use crate::sni_router;

use anyhow::Result;
use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};

pub trait ConnHandler {
    fn serve<IO>(
        &mut self,
        io: IO,
        addr: SocketAddr,
    ) -> impl std::future::Future<Output = Result<()>> + Send
    where
        IO: AsyncRead + AsyncWrite + Send + Unpin + 'static;
}

pub struct ConnHandlerParsedHelloConnHandler<S> {
    raw_conn_handler: S,
    tls_config: Arc<rustls::server::ServerConfig>,
}

impl<S> ConnHandlerParsedHelloConnHandler<S> {
    pub fn new(raw_conn_handler: S, tls_config: Arc<rustls::server::ServerConfig>) -> Self {
        Self {
            raw_conn_handler: raw_conn_handler,
            tls_config: tls_config,
        }
    }
}

/*
fn make_http12_config(rustls_server_config: Arc<rustls::server::ServerConfig>) -> RustlsConfig {
    let mut cloned = rustls_server_config.deref().clone();
    cloned.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    RustlsConfig::from_config(Arc::new(cloned))
}
*/

#[async_trait]
impl<S> sni_router::ParsedHelloConnHandler for ConnHandlerParsedHelloConnHandler<S>
where
    S: ConnHandler + Sync + Send + Clone,
{
    async fn handle(&self, conn: sni_router::ParsedHello, addr: SocketAddr) -> Result<()> {
        // TODO: protocol stuff?
        let acceptor = tokio_rustls::TlsAcceptor::from(self.tls_config.clone());
        let stream = acceptor.accept(conn.unread_conn()).await?;

        self.raw_conn_handler.clone().serve(stream, addr).await
    }
}

#[derive(Clone)]
struct AxumMakeServiceWrapper<M>
where
    M: Clone,
{
    make_service: M,
}

fn unwrap_infallible<T>(result: Result<T, std::convert::Infallible>) -> T {
    match result {
        Ok(value) => value,
        Err(err) => match err {},
    }
}

// based on the code in axum::Serve and the axum hyper example:
//
impl<M, S> ConnHandler for AxumMakeServiceWrapper<M>
where
    M: for<'a> tower_service::Service<SocketAddr, Error = Infallible, Response = S> + Send + Clone,
    M::Future: Send,
    S: tower_service::Service<
            axum::extract::Request<hyper::body::Incoming>,
            Response = axum::response::Response,
            Error = Infallible,
        > + Clone
        + Send
        + 'static,
    S::Future: Send,
{
    async fn serve<IO>(&mut self, io: IO, addr: SocketAddr) -> Result<()>
    where
        IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        use tower::ServiceExt;
        let tower_service = unwrap_infallible(self.make_service.call(addr).await);
        let hyper_service = hyper::service::service_fn(
            move |request: axum::extract::Request<hyper::body::Incoming>| {
                tower_service.clone().oneshot(request)
            },
        );

        let socket = hyper_util::rt::TokioIo::new(io);

        if let Err(err) =
            hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                .serve_connection_with_upgrades(socket, hyper_service)
                .await
        {
            // TODO: log
            // TODO: graceful shutdown? lol
            eprintln!("failed to serve connection: {err:#}");
        }

        Ok(())
    }
}

fn axum_make_service_to_conn_handler<M>(make_service: M) -> AxumMakeServiceWrapper<M>
where
    M: Clone,
    AxumMakeServiceWrapper<M>: ConnHandler,
{
    AxumMakeServiceWrapper {
        make_service: make_service,
    }
}

pub fn axum_router_to_conn_handler(router: axum::Router<()>) -> impl ConnHandler + Clone {
    axum_make_service_to_conn_handler(router.into_make_service_with_connect_info::<SocketAddr>())
}
