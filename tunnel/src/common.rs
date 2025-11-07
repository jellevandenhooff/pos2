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
    pub public_ip: String,
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

pub fn make_rustls_server_config(
    resolver: Arc<dyn rustls::server::ResolvesServerCert>,
) -> Result<Arc<rustls::ServerConfig>> {
    let rustls_config =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_cert_resolver(resolver);
    Ok(Arc::new(rustls_config))
}

#[derive(Clone)]
pub struct Environment {
    pub reqwest_client: reqwest::Client,
    pub acme_server_url: String,
    pub base_dir: String,
}

impl Environment {
    pub fn new(acme_server_url: String, base_dir: String) -> Result<Self> {
        // create rustls config with system certificates
        let mut root_store = rustls::RootCertStore::empty();
        let certs = rustls_native_certs::load_native_certs();
        for cert in certs.certs {
            root_store.add(cert).ok();
        }
        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(Arc::new(root_store))
            .with_no_client_auth();

        let reqwest_client = reqwest::Client::builder()
            .use_rustls_tls()
            .use_preconfigured_tls(tls_config)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        Ok(Self {
            reqwest_client,
            acme_server_url,
            base_dir,
        })
    }

    pub fn join_path(&self, path: &str) -> String {
        Path::join(Path::new(&self.base_dir), path)
            .to_string_lossy()
            .to_string()
    }

    pub async fn prod(base_dir: String) -> Result<Self> {
        Self::new(LetsEncrypt::Production.url().to_owned(), base_dir)
    }

    pub async fn test(base_dir: String) -> Result<Self> {
        Self::new("https://pebble:14000/dir".into(), base_dir)
    }
}
