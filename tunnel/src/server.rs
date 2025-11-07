use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use crate::common::Environment;
use crate::dnsserver::DnsClient;

type ConnectionsMap = Arc<Mutex<HashMap<String, quinn::Connection>>>;

pub fn make_connections_map() -> ConnectionsMap {
    Arc::new(Mutex::new(HashMap::<String, quinn::Connection>::new()))
}

pub struct TunnelServer {
    pub env: Environment,
    pub rustls_server_config: Arc<rustls::ServerConfig>,
    // quinn_listen_address:
    pub conns: ConnectionsMap,
    pub db: crate::db::DB,
}

impl TunnelServer {
    async fn run_handshake(
        self: Arc<Self>,
        conn: quinn::Incoming,
    ) -> Result<(quinn::Connection, String)> {
        let conn = conn.accept()?.await?;

        let (mut outgoing, incoming) = conn.accept_bi().await?;
        let mut buf = Vec::new();
        incoming.take(1024).read_to_end(&mut buf).await?;
        let hello: crate::common::ClientHello = serde_json::from_slice(&buf)?;

        tracing::info!("got client hello {:?}!", hello);

        let token = self
            .db
            .get_auth_token_by_id(&hello.token)
            .await?
            .context("need token")?;

        let user = self
            .db
            .get_user_by_id(&token.user_id)
            .await?
            .context("need user")?;

        if !user.domains.contains(&hello.domain) {
            tracing::info!("user not allowed");
            bail!("user not allowed");
        }

        // let binding = get_channel_binding(&conn);
        tracing::info!("server happy with handshake :)");

        let buf = serde_json::to_vec(&crate::common::ServerHello {})?;
        outgoing.write_all(&buf).await?;
        outgoing.finish()?;

        Ok((conn, hello.domain))
    }

    async fn handle_client(self: Arc<Self>, conn: quinn::Incoming) -> Result<()> {
        let (conn, domain) =
            timeout(Duration::from_secs(5), self.clone().run_handshake(conn)).await??;

        // store connection in map by domain, closing any previous connection for same domain
        {
            let mut conns = self.conns.lock().unwrap();
            let mut entry = conns.entry(domain.clone());
            entry = entry.and_modify(|prev| prev.close(quinn::VarInt::from(0u32), &[]));
            entry.insert_entry(conn.clone());
        }

        // wait for connection to close, then clean up from map
        {
            conn.closed().await;
            let mut conns = self.conns.lock().unwrap();
            if let Some(cur_conn) = conns.get(&domain) {
                if cur_conn.stable_id() == conn.stable_id() {
                    conns.remove(&domain);
                }
            }
        }

        Ok(())
    }

    pub async fn run(
        self: Arc<Self>,
        quinn_listen_addr: SocketAddr,
        stop: CancellationToken,
    ) -> Result<()> {
        let quinn_server_config = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(self.rustls_server_config.clone())?,
        ));

        // TODO: configurable??
        let server_endpoint = quinn::Endpoint::server(quinn_server_config, quinn_listen_addr)?;
        println!("listening");

        {
            let server_endpoint = server_endpoint.clone();
            tokio::spawn(async move {
                stop.cancelled().await;
                server_endpoint.close(quinn::VarInt::default(), &[]);
            });
        }

        loop {
            let conn = match server_endpoint.accept().await {
                Some(conn) => conn,
                None => {
                    return Ok(());
                }
            };

            let future = self.clone().handle_client(conn);
            tokio::task::spawn(async move {
                if let Err(err) = future.await {
                    println!("server_handle_client: {}", err);
                }
            });
        }
    }
}

#[async_trait]
impl crate::sni_router::ParsedHelloConnHandler for Arc<TunnelServer> {
    async fn handle(
        &self,
        tcp_conn: crate::sni_router::ParsedHello,
        addr: SocketAddr,
    ) -> Result<()> {
        // look up QUIC connection for this domain from SNI
        let conn = {
            let conns = self.conns.lock().unwrap();
            let hello = tcp_conn.client_hello();
            conns
                .get(hello.server_name().context("huh")?)
                .context("do not have server")?
                .clone()
        };

        // open bidirectional stream to client and send client socket address
        let addr_encoded = bincode::encode_to_vec(addr, bincode::config::standard()).unwrap();
        let addr_len = addr_encoded.len();

        let (mut tunnel_send, tunnel_recv) = conn.open_bi().await?;
        if addr_len > usize::from(u8::MAX) {
            bail!("wtf");
        }
        tunnel_send.write_u8(addr_len as u8).await?;
        tunnel_send.write_all(&addr_encoded).await?;

        // proxy traffic bidirectionally between tcp connection and tunnel stream
        let mut tunnel = tokio::io::join(tunnel_recv, tunnel_send);
        let mut tcp_conn = crate::timeout_stream::TimeoutStream::new(
            tcp_conn.unread_conn(),
            Duration::from_secs(15),
        );
        let _ = tokio::io::copy_bidirectional(&mut tcp_conn, &mut tunnel).await;

        println!("done copying...");

        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
pub struct ServerConfig {
    pub http_listen_addr: Option<String>, // port + addr? for api/web
    pub web_base_url: String,
    pub https_listen_addr: String,
    pub quinn_listen_addr: String,
    pub dns_listen_addr: String,
    pub dns_catalog_name: String,
    pub quinn_endpoint: String,
    pub hostname: String,
    pub public_ip: String, // net addr?
    pub available_suffixes: Vec<String>,
    pub github_oauth2_client_id: String,
    pub github_oauth2_client_secret: String,
    #[serde(default)]
    pub test_mode: bool,
}

struct ImportantTaskSet {
    stop: CancellationToken,
    tasks: tokio::task::JoinSet<()>,
}

impl ImportantTaskSet {
    fn new() -> Self {
        Self {
            stop: CancellationToken::new(),
            tasks: tokio::task::JoinSet::new(),
        }
    }

    fn spawn<A>(&mut self, name: &str, future: A)
    where
        A: Future<Output = Result<()>> + Send + 'static,
    {
        let name: String = name.into();
        let stop = self.stop.clone();
        self.tasks.spawn(async move {
            if let Err(err) = future.await {
                tracing::error!("task {} failed: {}", name, err);
            }
            stop.cancel();
        });
    }
}

pub async fn server_main(
    env: crate::common::Environment,
    server_config: ServerConfig,
) -> Result<()> {
    let mut tasks = ImportantTaskSet::new();

    let db = crate::db::DB::new(&env).await?;

    let web_router = crate::web::make_router(
        env.clone(),
        db.clone(),
        server_config.available_suffixes.clone(),
        server_config.web_base_url.clone(),
        server_config.github_oauth2_client_id.clone(),
        server_config.github_oauth2_client_secret.clone(),
        server_config.test_mode,
    )
    .await?;

    // TODO: only for local dev where we do not have a ssl cert?
    if let Some(http_listen_addr) = server_config.http_listen_addr {
        let web_router = web_router.clone();
        let stop = tasks.stop.clone();
        tasks.spawn("http listener", async move {
            let listener = tokio::net::TcpListener::bind(http_listen_addr).await?;
            axum::serve(listener, web_router.into_make_service())
                .with_graceful_shutdown(stop.cancelled_owned())
                .await?;
            Ok(())
        });
    }

    let hostname = &server_config.hostname;

    let (dns_server_api, mut dns_server_internals) = crate::dnsserver::make_dns_server(
        crate::dnsserver::Authorizer::new(db.clone()).await?,
        db.clone(),
        server_config.dns_catalog_name,
        server_config.dns_listen_addr.parse()?,
        server_config.quinn_endpoint.clone(),
        server_config.hostname.clone(),
    )
    .await?;
    {
        let stop = tasks.stop.clone();
        tasks.spawn("dns server", async move {
            let dns_future = dns_server_internals.block_until_done();
            let stop_future = stop.cancelled_owned();
            tokio::select! {
                result = dns_future => result?,
                _ = stop_future =>
                    dns_server_internals.shutdown_gracefully().await?
            }
            Ok(())
        });
    }

    // TODO: put any limits on what records this client can write?
    let dns_client: Arc<Box<dyn DnsClient>> = Arc::new(Box::new(dns_server_api.clone()));

    // TODO: public IP??
    dns_client
        .add_a_record(&format!("{hostname}."), &server_config.public_ip.parse()?)
        .await?;

    // TODO: put this maintainer in its own directory??
    let mut maintainer =
        crate::cert::CertMaintainer::initialize(env.clone(), vec![hostname.into()], dns_client)
            .await?;

    let config = crate::common::make_rustls_server_config(maintainer.cert_resolver())?;

    tokio::spawn(async move {
        // tasks.spawn("maintain certs", async move {
        maintainer.maintain_certs().await
    });

    let server = Arc::new(TunnelServer {
        env: env,
        rustls_server_config: config.clone(),
        conns: make_connections_map(),
        db: db,
    });

    {
        let stop = tasks.stop.clone();
        let server = server.clone();
        let listen_addr = server_config.quinn_listen_addr.parse()?;
        tokio::spawn(async move {
            // tasks.spawn("quinn server", async move {
            server.run(listen_addr, stop).await
        });
    }

    let mut sni_router = crate::sni_router::SniRouter {
        handlers: HashMap::new(),
        default: None,
    };

    let dns_router = crate::dnsserver::make_dns_router(dns_server_api.clone());

    let merged_router = dns_router.merge(web_router);

    let handler = crate::conn_handler::ConnHandlerParsedHelloConnHandler::new(
        crate::conn_handler::axum_router_to_conn_handler(merged_router),
        config.clone(),
    );
    sni_router
        .handlers
        .insert(hostname.into(), Box::new(handler));
    sni_router.default = Some(Box::new(server.clone()));

    // on :80, run a 443 redirect? (for known domains?)
    let tls_listener = tokio::net::TcpListener::bind(server_config.https_listen_addr).await?;
    tokio::spawn(async move {
        // tasks.spawn("tls listener", async move {
        crate::sni_router::handle_listener(tls_listener, Arc::new(sni_router)).await
    });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tasks.stop.cancel();
        },
    }

    // TODO: make all tasks stop gracefully
    // TODO: wait for all tasks to gracefully stop before dropping handle
    tasks.tasks.join_all().await;

    Ok(())
}
