use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use axum::extract::ConnectInfo;
use backoff::{SystemClock, backoff::Backoff};
use serde::{Deserialize, Serialize};
use tokio::{
    io::AsyncReadExt,
    time::{sleep, timeout},
};

use crate::dnsserver::extract_host;

pub struct TunnelClient<ConnHandler> {
    pub env: crate::common::Environment,
    pub tls_server_config: Arc<rustls::ServerConfig>,
    pub local_hostname: String, // TODO: support multiple
    pub tunnel_server_address: String,
    pub token: String,
    pub conn_handler: ConnHandler, // TODO: get rid of this generic
}

impl<ConnHandler> TunnelClient<ConnHandler>
where
    ConnHandler: crate::conn_handler::ConnHandler + Clone + Send + Sync + 'static,
{
    pub async fn get_tunnel_info(&self) -> Result<crate::common::TunnelInfoResponse> {
        let result = self
            .env
            .reqwest_client
            .get(format!("{0}/tunnel/info", self.tunnel_server_address))
            .bearer_auth(&self.token)
            .json(&crate::common::TunnelInfoRequest {})
            .send()
            .await
            .context("sending request to /tunnel/info")?
            .json::<crate::common::TunnelInfoResponse>()
            .await
            .context("receiving response from /tunnel/info")?;
        Ok(result)
    }

    async fn serve_conn(self: Arc<Self>, conn: quinn::Connection) -> Result<()> {
        loop {
            let (send, mut recv) = conn.accept_bi().await?;
            let addr_len = recv.read_u8().await.unwrap();
            let mut addr = vec![0; addr_len.into()];
            recv.read_exact(addr.as_mut_slice()).await.unwrap();
            let (parsed, _): (SocketAddr, _) =
                bincode::decode_from_slice(&addr, bincode::config::standard()).unwrap();
            println!("got parsed addr {:?}", parsed);

            // TODO: no unwrap please
            let io = tokio::io::join(recv, send);

            let config = self.tls_server_config.clone();
            let mut conn_handler = self.conn_handler.clone();

            tokio::spawn(async move {
                // TODO: protocol stuff?
                let acceptor = tokio_rustls::TlsAcceptor::from(config);
                let stream = acceptor.accept(io).await.unwrap();
                conn_handler.serve(stream, parsed).await.unwrap();
            });
        }
    }

    async fn open_conn(self: Arc<Self>) -> Result<quinn::Connection> {
        // TODO: reuse endpoint?
        let mut client_endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;

        let mut transport_config = quinn::TransportConfig::default();
        transport_config.keep_alive_interval(Some(Duration::from_secs(5)));
        let mut quinn_config =
            quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(
                self.env.rustls_client_config.clone(),
            )?));
        quinn_config.transport_config(Arc::new(transport_config));
        client_endpoint.set_default_client_config(quinn_config);

        let tunnel_info = self.get_tunnel_info().await?;
        println!("endpoint: {}", tunnel_info.quic_endpoint);

        let addrs = self
            .env
            .dns_resolver
            .resolve(&tunnel_info.quic_endpoint)
            .await?;
        let addr = addrs.into_iter().next().context("no addresses found")?;

        let tunnel_host = extract_host(&tunnel_info.quic_endpoint)?;

        // name here is for TLS
        let conn = client_endpoint.connect(addr, tunnel_host)?.await?;
        println!("made a connection!");

        let (mut outgoing, incoming) = conn.open_bi().await?;
        // XXX: binary encoding?
        let buf = serde_json::to_vec(&crate::common::ClientHello {
            domain: self.local_hostname.clone(),
            token: self.token.clone(),
        })?;
        outgoing.write_all(&buf).await?;
        outgoing.finish()?;
        drop(outgoing);

        let mut buf = Vec::new();
        incoming.take(1024).read_to_end(&mut buf).await?;
        let hello: crate::common::ServerHello = serde_json::from_slice(&buf)?;

        println!("got server hello {:?}", hello);

        Ok(conn)
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        let mut backoff = backoff::exponential::ExponentialBackoffBuilder::<SystemClock>::new()
            .with_initial_interval(Duration::from_secs(1))
            .with_max_interval(Duration::from_secs(30))
            .with_max_elapsed_time(None)
            .build();

        let mut initial = true;

        loop {
            if !initial {
                sleep(backoff.next_backoff().unwrap()).await;
            } else {
                initial = false;
            }

            let conn = match timeout(Duration::from_secs(10), self.clone().open_conn()).await {
                Ok(Ok(conn)) => conn,
                Ok(Err(err)) => {
                    println!("failed to connect {}", err);
                    continue;
                }
                Err(err) => {
                    println!("connecting timed out {}", err);
                    continue;
                }
            };

            backoff.reset();

            let future = self.clone().serve_conn(conn.clone());
            tokio::spawn(future);

            let err = conn.closed().await;
            println!("connectioned ended {}", err);
        }

        // Ok(())
    }
}

pub fn test_conn_handler() -> impl crate::conn_handler::ConnHandler + Clone {
    use axum::{Router, routing::get};
    let router = Router::new().route(
        "/",
        get(|ConnectInfo(addr): ConnectInfo<SocketAddr>| async move {
            format!("Hello, {}!", addr.clone())
        }),
    );

    crate::conn_handler::axum_router_to_conn_handler(router)
}

pub const DEFAULT_ENDPOINTS: &[&str] = &["https://tunnel.nunya.cloud"];

#[derive(Serialize, Deserialize, Debug)]
pub struct ClientState {
    pub endpoint: String,
    pub token: String,
    pub domain: String,
}

#[derive(Serialize, Deserialize)]
pub struct ClientConfig {
    pub endpoints: Vec<String>,
}

pub async fn client_main(
    env: crate::common::Environment,
    handler: impl crate::conn_handler::ConnHandler + Clone + Send + Sync + 'static,
    client_config: ClientConfig,
) -> Result<()> {
    let (endpoint, domain, token) = if let Some(state) =
        crate::common::read_optional_json_file::<ClientState>(&env.join_path("state.json")).await?
    {
        (state.endpoint, state.domain, state.token)
    } else {
        let state = crate::setup::run_interactive_setup(&env, client_config.endpoints).await?;

        crate::common::write_json_file(&env.join_path("state.json"), &state).await?;

        (state.endpoint, state.domain, state.token)
    };

    let name = &domain;
    let secret = token;

    // TODO: make the DNS client an API client instead
    let dns_client: Arc<Box<dyn crate::dnsserver::DnsClient>> =
        Arc::new(Box::new(crate::dnsserver::DnsServerClient {
            env: env.clone(),
            url: endpoint.clone(),
            secret: secret.clone(),
        }));

    let mut maintainer = crate::cert::CertMaintainer::initialize(
        env.clone(),
        vec![
            // TODO: wildcard child??
            name.into(),
        ],
        dns_client.clone(),
    )
    .await?;

    let cert_resolver = maintainer.cert_resolver();

    // TODO: how does this log?
    tokio::spawn(async move { maintainer.maintain_certs().await });

    let client = Arc::new(TunnelClient {
        env: env,
        tls_server_config: crate::common::make_rustls_server_config(cert_resolver)?,
        conn_handler: handler,
        local_hostname: name.into(),
        tunnel_server_address: endpoint.into(), // TODO: get from API
        token: secret.clone(),
    });

    // TODO: only get once? update DNS over and over?
    let tunnel_info = client.get_tunnel_info().await?;

    dns_client
        .add_cname_record(&format!("{name}."), &tunnel_info.public_endpoint)
        .await?;

    client.run().await?;

    Ok(())
}
