use std::net::SocketAddr;
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json};
use axum_extra::{
    TypedHeader,
    headers::{Authorization, authorization::Bearer},
};
use chrono::Utc;
use hickory_server::proto::rr::RecordData;
use hickory_server::{
    authority::{Authority, Catalog, MessageResponseBuilder, ZoneType},
    proto::{
        op::{Header, MessageType, OpCode, ResponseCode},
        rr::{
            LowerName, Name, RData, Record, RecordSet, RecordType, RrKey,
            rdata::{A, CNAME, SOA, TXT},
        },
        udp::UdpSocket,
    },
    server::{Request, RequestHandler, ResponseHandler, ResponseInfo},
    store::in_memory::InMemoryAuthority,
};
use std::str::FromStr;

// XXX pub
pub struct ReadonlyRequestHandler<T: RequestHandler> {
    underlying: T,
}

impl<T> ReadonlyRequestHandler<T>
where
    T: RequestHandler,
{
    fn new(inner: T) -> Self {
        Self { underlying: inner }
    }
}

fn check_readonly(request: &Request) -> Result<()> {
    match request.message_type() {
        MessageType::Query => match request.op_code() {
            OpCode::Query => Ok(()),
            OpCode::Update => {
                bail!("it is an update")
                // no
            }
            OpCode::Status | OpCode::Notify | OpCode::Unknown(_) => Ok(()),
        },
        MessageType::Response => Ok(()),
    }
}

#[async_trait::async_trait]
impl<T> RequestHandler for ReadonlyRequestHandler<T>
where
    T: RequestHandler,
{
    async fn handle_request<R: ResponseHandler>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        println!("req {:?}", request);

        if let Err(bad) = check_readonly(request) {
            println!("rejecting request: {}", bad);

            let response = MessageResponseBuilder::from_message_request(request);

            let result = response_handle
                .send_response(response.error_msg(request.header(), ResponseCode::NotImp))
                .await;

            return match result {
                Err(e) => {
                    println!("request failed: {}", e);
                    // error!("request failed: {}", e);
                    let mut header = Header::new();
                    header.set_response_code(ResponseCode::ServFail);
                    header.into()
                }
                Ok(info) => info,
            };
        }

        let response_info = self.underlying
            .handle_request(request, response_handle)
            .await;

        response_info
    }
}

pub struct DnsServer {
    authority: Arc<InMemoryAuthority>,
    authorizer: Authorizer,
    peristence: crate::db::DB,
    quic_tunnel_endpoint: String,
    external_domain: String,
    public_ip: String,
}

pub struct Authorizer {
    db: crate::db::DB,
}

impl Authorizer {
    pub async fn new(db: crate::db::DB) -> Result<Self> {
        Ok(Self { db: db })
    }

    async fn can_write(&self, token: &str, domain: &str) -> Result<()> {
        // TODO: tx?
        let token = self
            .db
            .get_auth_token_by_id(token)
            .await?
            .context("missing token")?;
        let user = self
            .db
            .get_user_by_id(&token.user_id)
            .await?
            .context("missing user")?;

        let mut domain = domain.strip_suffix(".").context("should end in .")?;

        if let Some(trimmed) = domain.strip_prefix("_acme-challenge.") {
            domain = trimmed;
        } else if let Some(trimmed) = domain.strip_prefix("*.") {
            domain = trimmed;
        } else {
            // keep
        }

        // TODO: log trimmed?

        if user.domains.iter().find(|s| *s == domain).is_none() {
            bail!("not allowed");
        }
        Ok(())
    }
}

#[async_trait]
pub trait DnsClient: Send + Sync {
    async fn add_txt_record(&self, name: &str, value: &str) -> Result<()>;
    async fn add_a_record(&self, name: &str, value: &std::net::Ipv4Addr) -> Result<()>;
    async fn add_cname_record(&self, name: &str, value: &str) -> Result<()>;
}

async fn add_record(
    dns_server: &Arc<DnsServer>,
    name: &str,
    ttl: i32,
    expires_at: Option<chrono::DateTime<Utc>>, // TODO
    rdata: RData,
) -> Result<()> {
    let name = Name::from_str(name)?;
    let name = LowerName::from(name);

    let encoded = serde_json::to_string(&rdata)?;

    dns_server
        .peristence
        .write_dns_record(
            &name.to_string(),
            rdata.record_type().into(),
            &encoded,
            ttl,
            expires_at,
        )
        .await?;

    dns_server
        .authority
        .clone()
        .upsert(
            Record::from_rdata(Name::from(name), ttl.try_into()?, rdata),
            1, // XXX
        )
        .await;
    Ok(())
}

#[async_trait]
impl DnsClient for Arc<DnsServer> {
    async fn add_txt_record(&self, name: &str, value: &str) -> Result<()> {
        let rdata = TXT::new(vec![value.into()]).into_rdata();
        add_record(self, name, 1, None, rdata).await?;
        Ok(())
    }

    async fn add_a_record(&self, name: &str, value: &std::net::Ipv4Addr) -> Result<()> {
        let rdata = A(value.clone().into()).into_rdata();
        add_record(self, name, 1, None, rdata).await?;
        Ok(())
    }

    async fn add_cname_record(&self, name: &str, value: &str) -> Result<()> {
        if !value.ends_with('.') {
            bail!("cname target must end with '.' to be fully qualified");
        }
        let rdata = CNAME(Name::from_str(value)?).into_rdata();
        add_record(self, name, 1, None, rdata).await?;
        Ok(())
    }
}

pub async fn make_dns_server(
    authorizer: Authorizer,
    persistence: crate::db::DB,
    dns_catalog_name: String,
    listen_address: SocketAddr,
    quic_tunnel_endpoint: String,
    external_domain: String,
    public_ip: String,
) -> Result<(
    Arc<DnsServer>,
    hickory_server::ServerFuture<ReadonlyRequestHandler<Catalog>>,
)> {
    let mut catalog = Catalog::new();

    let name = LowerName::from(Name::from_str(&dns_catalog_name)?);

    let fixed_serial_number = 1;

    let mut record_set = RecordSet::new(name.clone().into(), RecordType::SOA, 1);
    record_set.insert(
        Record::from_rdata(
            name.clone().into(),
            60,
            // TODO: sane values here
            RData::SOA(SOA::new(
                name.clone().into(),
                name.clone().into(),
                fixed_serial_number,
                60,
                60,
                60,
                60,
            )),
        ),
        fixed_serial_number,
    );

    let authority = Arc::new(
        InMemoryAuthority::new(
            name.clone().into(),
            BTreeMap::from_iter(
                vec![(
                    RrKey::new(
                        LowerName::from(record_set.name().clone()),
                        record_set.record_type(),
                    ),
                    record_set,
                )]
                .into_iter(),
            ),
            ZoneType::Primary,
            false,
        )
        .expect("could not create huh"),
    );
    let origin = authority.origin().clone();
    catalog.upsert(origin, vec![authority.clone()]);

    let readonly_handler = ReadonlyRequestHandler::new(catalog);
    let mut server = hickory_server::ServerFuture::new(readonly_handler);
    println!("listeneing");
    let socket = UdpSocket::bind(listen_address).await?;
    server.register_socket(socket);
    println!("registered");

    let records = persistence.get_records().await?;
    for record in records {
        // TODO: skip broken records?
        let name = LowerName::from(Name::from_str(&record.name)?);
        let rdata: RData = serde_json::from_str(&record.value)?;
        authority
            .upsert(
                Record::from_rdata(Name::from(name), record.ttl_seconds.try_into()?, rdata),
                1,
            )
            .await;
    }

    // server.block_until_done().await?;
    // println!("done");
    Ok((
        Arc::new(DnsServer {
            authority: authority,
            authorizer: authorizer,
            peristence: persistence,
            quic_tunnel_endpoint: quic_tunnel_endpoint,
            external_domain: external_domain,
            public_ip: public_ip,
        }),
        server,
    ))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DnsSetTxtRequest {
    name: String,
    value: String,
}

// TODO: endpoint to delete records? list them?
// TODO: A record? AAAA?

fn check_txt_value(value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    if bytes.len() > 64 {
        bail!("too long");
    }
    for byte in bytes {
        if *byte < 32 || *byte > 126 {
            bail!("illegal byte value");
        }
    }
    Ok(())
}

fn check_cname_value(value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    if bytes.len() > 128 {
        bail!("too long");
    }
    Name::from_str(value)?;
    Ok(())
}

async fn dns_set_txt_handler(
    Extension(dnsserver): Extension<Arc<DnsServer>>,
    TypedHeader(Authorization(creds)): TypedHeader<Authorization<Bearer>>,
    Json(payload): Json<DnsSetTxtRequest>,
) -> Response {
    println!("bearer: {:?}", creds.token());

    dnsserver
        .authorizer
        .can_write(creds.token(), &payload.name)
        .await
        .expect("bad token");

    check_txt_value(&payload.value).unwrap();

    // TODO: check cname value

    // TODO: delete old/timeout?

    // TODO: error handling
    // TODO: auth
    dnsserver
        .add_txt_record(&payload.name, &payload.value)
        .await
        .unwrap();

    Json("").into_response()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DnsSetARequest {
    name: String,
    value: std::net::Ipv4Addr,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DnsSetCnameRequest {
    name: String,
    value: String,
}

async fn dns_set_a_handler(
    Extension(dnsserver): Extension<Arc<DnsServer>>,
    TypedHeader(Authorization(creds)): TypedHeader<Authorization<Bearer>>,
    Json(payload): Json<DnsSetARequest>,
) -> Response {
    println!("bearer: {:?}", creds.token());

    dnsserver
        .authorizer
        .can_write(creds.token(), &payload.name)
        .await
        .expect("bad token");

    // check_txt_value(&payload.value).unwrap();

    // TODO: delete old/timeout?

    // TODO: error handling
    // TODO: auth
    dnsserver
        .add_a_record(&payload.name, &payload.value)
        .await
        .unwrap();

    Json("").into_response()
}

async fn dns_set_cname_handler(
    Extension(dnsserver): Extension<Arc<DnsServer>>,
    TypedHeader(Authorization(creds)): TypedHeader<Authorization<Bearer>>,
    Json(payload): Json<DnsSetCnameRequest>,
) -> Response {
    println!("bearer: {:?}", creds.token());

    dnsserver
        .authorizer
        .can_write(creds.token(), &payload.name)
        .await
        .expect("bad token");

    check_cname_value(&payload.value).unwrap();

    // TODO: delete old/timeout?

    // TODO: error handling
    // TODO: auth
    dnsserver
        .add_cname_record(&payload.name, &payload.value)
        .await
        .unwrap();

    Json("").into_response()
}

async fn get_tunnel_info(Extension(dnsserver): Extension<Arc<DnsServer>>) -> Response {
    Json(crate::common::TunnelInfoResponse {
        quic_endpoint: dnsserver.quic_tunnel_endpoint.clone(),
        public_endpoint: dnsserver.external_domain.clone(),
        public_ip: dnsserver.public_ip.clone(),
    })
    .into_response()
}

pub fn make_dns_router(dnsserver: Arc<DnsServer>) -> axum::Router {
    axum::Router::new()
        .route("/dns/set/txt", post(dns_set_txt_handler))
        .route("/dns/set/a", post(dns_set_a_handler))
        .route("/dns/set/cname", post(dns_set_cname_handler))
        .route("/tunnel/info", get(get_tunnel_info))
        .layer(Extension(dnsserver))
}

pub struct DnsServerClient {
    pub env: crate::common::Environment,
    pub url: String,
    pub secret: String,
}

#[async_trait]
impl DnsClient for DnsServerClient {
    async fn add_txt_record(&self, name: &str, value: &str) -> Result<()> {
        // TODO: dedup

        // TODO: error handling
        let _result = self
            .env
            .reqwest_client
            .post(format!("{0}/dns/set/txt", self.url))
            .bearer_auth(&self.secret)
            // .header("Authorization", format!("Bearer {0}", self.secret))
            .json(&DnsSetTxtRequest {
                name: name.into(),
                value: value.into(),
            })
            .send()
            .await
            .context("sending request to /dns/set/txt")?
            .json::<String>()
            .await
            .context("receiving response from /dns/set/txt")?;
        Ok(())
    }

    async fn add_a_record(&self, name: &str, value: &std::net::Ipv4Addr) -> Result<()> {
        // TODO: dedup

        // TODO: error handling
        let _result = self
            .env
            .reqwest_client
            .post(format!("{0}/dns/set/a", self.url))
            .bearer_auth(&self.secret)
            .json(&DnsSetARequest {
                name: name.into(),
                value: value.clone(),
            })
            .send()
            .await
            .context("sending request to /dns/set/a")?
            .json::<String>()
            .await
            .context("receiving response from /dns/set/a")?;
        Ok(())
    }

    async fn add_cname_record(&self, name: &str, value: &str) -> Result<()> {
        // TODO: dedup

        // TODO: error handling
        let _result = self
            .env
            .reqwest_client
            .post(format!("{0}/dns/set/cname", self.url))
            .bearer_auth(&self.secret)
            // .header("Authorization", format!("Bearer {0}", self.secret))
            .json(&DnsSetCnameRequest {
                name: name.into(),
                value: value.into(),
            })
            .send()
            .await
            .context("sending request to /dns/set/cname")?
            .json::<String>()
            .await
            .context("receiving response from /dns/set/cname")?;
        Ok(())
    }
}

pub fn extract_host(host_and_port: &str) -> Result<&str> {
    let (host, _port) = host_and_port.rsplit_once(':').context("missing :")?;
    Ok(host)
}
