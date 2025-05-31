use std::{io::Cursor, path::Path, sync::Arc};

use anyhow::{Result, bail};
use instant_acme::LetsEncrypt;
use serde::{Deserialize, Serialize};
use tokio::io;

#[derive(Serialize, Deserialize, Debug)]
pub struct ClientHello {
    // version? other junk?
    pub domain: String,
    pub token: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ServerHello {
    // info? other junk?
    // domain: String,
}

#[derive(Serialize, Deserialize)]
pub struct TunnelInfoRequest {}

#[derive(Serialize, Deserialize)]
pub struct TunnelInfoResponse {
    pub quic_endpoint: String,
    pub public_endpoint: String,
}

pub async fn read_optional_json_file<T>(path: &str) -> Result<Option<T>>
where
    T: serde::de::DeserializeOwned,
{
    let parsed = match read_optional_file(path).await? {
        None => None,
        Some(contents) => Some(serde_json::from_reader(Cursor::new(contents))?),
    };
    Ok(parsed)
}

pub async fn read_json_file<T>(path: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    match read_optional_json_file(path).await? {
        None => bail!("missing file"),
        Some(parsed) => Ok(parsed),
    }
}

pub async fn write_json_file<T>(path: &str, value: &T) -> Result<()>
where
    T: serde::Serialize,
{
    tokio::fs::write(path, serde_json::to_string_pretty(value)?).await?;
    Ok(())
}

pub async fn read_optional_file(path: &str) -> Result<Option<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => Ok(Some(contents)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => {
            bail!("reading {path}: {err}");
        }
    }
}

#[allow(unused)]
fn make_rustls_client_config() -> Result<rustls::ClientConfig> {
    use rustls_platform_verifier::BuilderVerifierExt;

    Ok(
        rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_platform_verifier()?
            .with_no_client_auth(),
    )
}

pub fn make_rustls_server_config(
    resolver: Arc<dyn rustls::server::ResolvesServerCert>,
) -> Result<Arc<rustls::ServerConfig>> {
    // let resolver = cert::StaticCertResolver::load_from_files(name)?;
    let rustls_config =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_cert_resolver(resolver);
    Ok(Arc::new(rustls_config))
}

#[derive(Clone)]
pub struct Environment {
    pub rustls_client_config: Arc<rustls::ClientConfig>,
    // pub rustls_server_config: Arc<rustls::ServerConfig>,
    pub dns_resolver: Arc<dyn crate::dnsserver::BasicResolver>,
    pub reqwest_client: reqwest::Client,
    pub external_reqwest_client: reqwest::Client,
    pub acme_server_url: String,
    pub base_dir: String,
}

impl Environment {
    pub fn new(
        rustls_client_config: rustls::ClientConfig,
        // rustls_server_config: rustls::ServerConfig,
        dns_resolver: Arc<dyn crate::dnsserver::BasicResolver>,
        acme_server_url: String,
        base_dir: String,
    ) -> Result<Self> {
        let reqwest_client = reqwest::Client::builder()
            .use_rustls_tls()
            .use_preconfigured_tls(rustls_client_config.clone())
            .dns_resolver(Arc::new(crate::dnsserver::ResolverWrapper {
                inner: dns_resolver.clone(),
            }))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        let external_reqwest_client = reqwest::Client::builder()
            .use_rustls_tls()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        Ok(Self {
            rustls_client_config: Arc::new(rustls_client_config),
            // rustls_server_config: Arc::new(rustls_server_config),
            dns_resolver: dns_resolver,
            reqwest_client,
            external_reqwest_client,
            acme_server_url: acme_server_url,
            base_dir: base_dir,
        })
    }

    pub fn join_path(&self, path: &str) -> String {
        Path::join(Path::new(&self.base_dir), path)
            .to_string_lossy()
            .to_string()
    }

    pub async fn prod(base_dir: String) -> Result<Self> {
        Self::new(
            make_rustls_client_config()?,
            Arc::new(crate::dnsserver::BuiltinResolver {}),
            LetsEncrypt::Production.url().to_owned(),
            base_dir,
        )
    }

    pub async fn test(base_dir: String) -> Result<Self> {
        // UGH... TODO, make this work with real github (maybe? somehow?)
        Self::new(
            crate::cert::make_test_client_tlsconfig().await?,
            // rustls_server_config,
            Arc::new(crate::dnsserver::HickoryResolver::new()?),
            "https://localhost:14000/dir".into(),
            base_dir,
        )
    }
}
