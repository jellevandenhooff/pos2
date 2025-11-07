use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use instant_acme::{
    Account, AuthorizationStatus, CertificateIdentifier, ChallengeType,
    Identifier, NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;
use rustls::crypto::CryptoProvider;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::time::sleep;

use crate::dnsserver::{self};

// TODO: ipv6
// docker run --rm -it -p 14000:14000 -p 15000:15000 -e "PEBBLE_VA_NOSLEEP=1 PEBBLE_WFE_NONCEREJECT=0" ghcr.io/letsencrypt/pebble -dnsserver host.docker.internal:9999
// PEBBLE_VA_NOSLEEP=1 PEBBLE_WFE_NONCEREJECT=0 go run ./cmd/pebble -dnsserver 127.0.0.1:9999

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateInfo {
    #[serde(with = "time::serde::rfc3339")]
    pub renew_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub update_renew_info_at: time::OffsetDateTime,
}

fn cert_file_name(env: &crate::common::Environment, cert_name: &str) -> String {
    env.join_path(&format!("acme/{cert_name}.pem"))
}

fn key_file_name(env: &crate::common::Environment, cert_name: &str) -> String {
    env.join_path(&format!("acme/{cert_name}-key.pem"))
}

fn info_file_name(env: &crate::common::Environment, cert_name: &str) -> String {
    env.join_path(&format!("acme/{cert_name}-info.json"))
}

async fn write_private_file(path: &str, contents: &[u8]) -> Result<()> {
    let mut f = tokio::fs::File::options()
        .mode(0o600)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .await?;
    f.write_all(contents).await?;
    f.sync_all().await?;
    Ok(())
}

async fn sync_dir(path: &str) -> Result<()> {
    tokio::fs::File::open(path).await?.sync_all().await?;
    Ok(())
}

#[derive(Debug)]
pub struct LoadedCertificate {
    pub info: CertificateInfo,
    pub cert: String,
    pub key: String,
}

async fn load_certs(
    env: &crate::common::Environment,
    name: &str,
) -> Result<Option<LoadedCertificate>> {
    let info: CertificateInfo =
        match crate::common::read_optional_json_file(&info_file_name(env, name)).await? {
            Some(info) => info,
            None => return Ok(None),
        };
    let cert = match crate::common::read_optional_file(&cert_file_name(env, name)).await? {
        Some(cert) => cert,
        None => return Ok(None),
    };
    let key = match crate::common::read_optional_file(&key_file_name(env, name)).await? {
        Some(key) => key,
        None => return Ok(None),
    };
    Ok(Some(LoadedCertificate {
        info: info,
        cert: cert,
        key: key,
    }))
}

async fn write_certs(
    env: &crate::common::Environment,
    name: &str,
    cert: &LoadedCertificate,
) -> Result<()> {
    // TODO: if some writes succeed and others fail we are in a world of trouble

    tokio::fs::write(
        info_file_name(env, name),
        serde_json::to_string_pretty(&cert.info)?,
    )
    .await?;
    tokio::fs::write(&cert_file_name(env, name), cert.cert.as_bytes()).await?;
    write_private_file(&key_file_name(env, name), cert.key.as_bytes()).await?;
    sync_dir(&env.join_path("acme")).await?;
    Ok(())
}

pub async fn getaccount(env: &crate::common::Environment) -> Result<Account> {
    let credentials_path = env.join_path("acme/credentials.json");

    let credentials = crate::common::read_optional_json_file(&credentials_path)
        .await
        .context("reading ./acme/credentials.json")?;

    // try logging in with the credentials
    let account = match credentials {
        Some(credentials) => {
            match Account::builder()?.from_credentials(credentials).await {
                Ok(result) => {
                    println!("logged in successfully...");
                    result
                }
                /*
                // TODO: this does not work here because we don't actually validate the account...
                Err(instant_acme::Error::Api(instant_acme::Problem {
                    r#type: Some(type_str),
                    ..
                })) if type_str == "urn:ietf:params:acme:error:accountDoesNotExist" => {
                    println!("account did not exist...?");
                    None
                }
                */
                Err(err) => {
                    bail!("getting account: {}", err);
                }
            }
        }
        None => {
            // let (account, credentials) =
            let (account, credentials) = Account::builder()?
                .create(
                    &NewAccount {
                        contact: &[],
                        terms_of_service_agreed: true,
                        only_return_existing: false,
                    },
                    env.acme_server_url.clone(),
                    None,
                )
                .await?;

            tokio::fs::write(
                &credentials_path,
                serde_json::to_string_pretty(&credentials)?,
            )
            .await?;

            account
        }
    };

    Ok(account)
}

pub async fn ordercert(
    env: &crate::common::Environment,
    account: &Account,
    dns_server: &Arc<Box<dyn dnsserver::DnsClient>>,
    name: &String,
    domains: &Vec<String>,
    previous: Option<&LoadedCertificate>,
) -> Result<LoadedCertificate> {
    let identifiers = domains
        .iter()
        .map(|ident| Identifier::Dns(ident.clone()))
        .collect::<Vec<_>>();

    let mut new_order = NewOrder::new(identifiers.as_slice());
    if let Some(previous) = previous {
        // TODO: check account matches?
        let id = get_certificate_id(&previous.cert)?;
        new_order = new_order.replaces(id);
    }

    // TODO: replaces here?
    let mut order = account.new_order(&new_order).await?;

    let state = order.state();
    println!("order state: {:#?}", state);
    // assert!(matches!(state.status, OrderStatus::Pending));

    // Pick the desired challenge type and prepare the response.

    // complete DNS-01 challenges for each domain
    let mut authorizations = order.authorizations();
    while let Some(result) = authorizations.next().await {
        let mut authz = result?;
        match authz.status {
            AuthorizationStatus::Pending => {}
            AuthorizationStatus::Valid => continue,
            _ => todo!(),
        }

        // We'll use the DNS challenges for this example, but you could
        // pick something else to use here.

        let mut challenge = authz
            .challenge(ChallengeType::Dns01)
            .ok_or_else(|| anyhow::anyhow!("no dns01 challenge found"))?;

        let dns = match challenge.identifier().identifier {
            Identifier::Dns(dns) => dns,
            _ => panic!("help"),
        };

        println!("setting dns challenge...");
        println!(
            "_acme-challenge.{} IN TXT {}",
            dns,
            challenge.key_authorization().dns_value()
        );

        // TODO: here we are running the challenges serially. makes sense, i think. that means we
        // can cap the number of records at 1?

        // write TXT record to DNS server for ACME validation
        dns_server
            .add_txt_record(
                &format!("_acme-challenge.{}.", dns),
                &challenge.key_authorization().dns_value(),
            )
            .await?;

        // sleep(Duration::from_secs(120)).await;

        // io::stdin().read_line(&mut String::new())?;

        challenge.set_ready().await?;
    }

    // Exponentially back off until the order becomes ready or invalid.

    let status = order.poll_ready(&RetryPolicy::new()).await?;
    if status != OrderStatus::Ready {
        return Err(anyhow::anyhow!("unexpected order status: {status:?}"));
    }

    // Finalize the order and print certificate chain, private key and account credentials.

    let private_key_pem = order.finalize().await?;
    let cert_chain_pem = loop {
        match order.certificate().await? {
            Some(cert_chain_pem) => break cert_chain_pem,
            None => sleep(Duration::from_secs(1)).await,
        }
    };

    let info = get_renewal_info(account, &cert_chain_pem).await?;

    let packed = LoadedCertificate {
        info: info,
        cert: cert_chain_pem,
        key: private_key_pem,
    };

    write_certs(env, &name, &packed).await?;

    Ok(packed)
}

fn get_certificate_id(certificate: &String) -> Result<CertificateIdentifier<'_>> {
    let certificate = CertificateDer::pem_slice_iter(certificate.as_bytes())
        .into_iter()
        .next()
        .context("missing certificate")??;

    Ok(CertificateIdentifier::try_from(&certificate)
        .or_else(|reason| bail!("help {:?}", reason))?)
}

async fn get_renewal_info(account: &Account, cert: &String) -> Result<CertificateInfo> {
    let id = get_certificate_id(cert)?;
    let (renewal_info, _refetch_duration_hint) = account.renewal_info(&id).await?;

    // renew: handle AlreadyReplaced
    // renew: handle mismatched account
    // renew: don't check if certificate already expired (

    let now = OffsetDateTime::now_utc();
    let check_next = (now + time::Duration::hours(1))
        .replace_nanosecond(0)
        .unwrap();

    let window = &renewal_info.suggested_window;
    let diff = (window.end - window.start).whole_seconds();
    let offset = time::Duration::seconds(StdRng::from_os_rng().random_range(0..diff));
    let renewal_time = renewal_info.suggested_window.start + offset;

    let cert_info = CertificateInfo {
        renew_at: renewal_time,
        update_renew_info_at: check_next,
    };

    Ok(cert_info)
}

fn sleep_until_offsetdatetime(target_time: &OffsetDateTime) -> tokio::time::Sleep {
    let now = OffsetDateTime::now_utc();
    let duration = (*target_time - now).try_into().unwrap_or(Duration::ZERO);
    sleep(duration)
}

pub struct CertMaintainer {
    account: Account,
    dns_server: Arc<Box<dyn dnsserver::DnsClient>>,
    acceptor: Arc<UpdatingCertResolver>,
    domains: Vec<String>,
    loaded: LoadedCertificate,
    name: String,
    env: crate::common::Environment,
}

impl CertMaintainer {
    pub async fn initialize(
        env: crate::common::Environment,
        mut domains: Vec<String>,
        dns_server: Arc<Box<dyn dnsserver::DnsClient>>,
    ) -> Result<CertMaintainer> {
        // Create a new account. This will generate a fresh ECDSA key for you.
        // Alternatively, restore an account from serialized credentials by
        // using `Account::from_credentials()`.
        println!("hello");

        fs::create_dir_all(&env.join_path("./acme")).await?;

        let account = getaccount(&env).await?;

        // TODO: check that we can write to this directoyr before we try and acquire certificates?
        //
        // Create the ACME order based on the given domain names.
        // Note that this only needs an `&Account`, so the library will let you
        // process multiple orders in parallel for a single account.
        domains.sort();

        let identifiers = domains
            .iter()
            .map(|ident| Identifier::Dns(ident.clone()))
            .collect::<Vec<_>>();
        let name = identifiers
            .iter()
            .map(|id| format!("{:?}", id).to_string())
            .collect::<Vec<_>>()
            .join("-");

        println!("name: {}", name);

        let loaded = match load_certs(&env, &name).await? {
            Some(loaded) => loaded,
            None => ordercert(&env, &account, &dns_server, &name, &domains, None).await?,
        };

        // read info, crt, key
        // run watchdog renewer

        // let order = account.renewal_info(_).await?;
        // println!("certificate chain:\n\n{cert_chain_pem}");
        // println!("private key:\n\n{private_key_pem}");
        //
        let acceptor = UpdatingCertResolver::new(&loaded)?;

        Ok(CertMaintainer {
            account: account,
            dns_server: dns_server,
            domains: domains,
            name: name,
            loaded: loaded,
            acceptor: Arc::new(acceptor),
            env: env,
        })
    }

    pub fn cert_resolver(&self) -> Arc<UpdatingCertResolver> {
        return self.acceptor.clone();
    }

    pub async fn maintain_certs(&mut self) -> Result<()> {
        // background loop to monitor renewal windows and renew certificates
        // TODO: retry on fail?
        loop {
            loop {
                let renew_at = &self.loaded.info.renew_at;
                let update_renew_info_at = &self.loaded.info.update_renew_info_at;

                tokio::select! {
                    // TODO: help negative time
                    _ = sleep_until_offsetdatetime(renew_at) => {

                        break
                    },
                    _ = sleep_until_offsetdatetime(update_renew_info_at) => {
                        self.loaded.info = get_renewal_info(&self.account, &self.loaded.cert).await?;
                        // TODO: write to disk?
                    },
                };
            }

            // renew certificate via ACME with replaces parameter
            // TODO: log
            let newinfo = ordercert(
                &self.env,
                &self.account,
                &self.dns_server,
                &self.name,
                &self.domains,
                Some(&self.loaded),
            )
            .await?;

            // hot-swap new certificate into TLS config
            self.acceptor.update_key(&newinfo)?;
            self.loaded = newinfo;
        }
    }
}

fn parse_cert_and_key(
    info: &LoadedCertificate,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let mut certs = vec![];
    // unclear if these into_owned and clone_key are necessary...?
    for cert in CertificateDer::pem_slice_iter(info.cert.as_bytes()) {
        certs.push(cert?.into_owned());
    }
    let key = PrivateKeyDer::from_pem_slice(info.key.as_bytes())?.clone_key();
    Ok((certs, key))
}

#[derive(Debug)]
pub struct UpdatingCertResolver {
    // config: Mutex<Arc<ServerConfig>>,
    key: Mutex<Option<Arc<rustls::sign::CertifiedKey>>>,
}

impl UpdatingCertResolver {
    pub fn new(info: &LoadedCertificate) -> Result<Self> {
        // TODO: huh
        // initial.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let res = Self {
            // config: Mutex::new(Arc::new(initial)),
            key: Mutex::new(None),
        };
        res.update_key(info)?;
        Ok(res)
    }

    pub fn update_key(&self, info: &LoadedCertificate) -> Result<()> {
        let (cert_chain, key) = parse_cert_and_key(info)?;
        let key = rustls::sign::CertifiedKey::from_der(
            cert_chain,
            key,
            CryptoProvider::get_default().unwrap(),
        )?;
        *self.key.lock().unwrap() = Some(Arc::new(key));
        Ok(())
    }
}

impl ResolvesServerCert for UpdatingCertResolver {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        let guard = self.key.lock().unwrap();
        return guard.clone();
    }
}

#[allow(unused)]
#[derive(Debug)]
pub struct StaticCertResolver {
    key: Option<Arc<rustls::sign::CertifiedKey>>,
}

#[allow(unused)]
impl StaticCertResolver {
    pub fn new(certs: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> Result<Self> {
        let key = rustls::sign::CertifiedKey::from_der(
            certs,
            key,
            CryptoProvider::get_default().unwrap(),
        )?;

        Ok(Self {
            key: Some(Arc::new(key)),
        })
    }

    pub fn load_from_files(name: &str) -> Result<Self> {
        let mut certs = vec![];
        for cert in CertificateDer::pem_file_iter(format!("{}.pem", name))? {
            certs.push(cert?.into_owned());
        }
        let key = PrivateKeyDer::from_pem_file(format!("{}-key.pem", name))?.clone_key();
        Self::new(certs, key)
    }
}

impl ResolvesServerCert for StaticCertResolver {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        self.key.clone()
    }
}

/*
fn make_rustls_server_config(info: &LoadedCertificate) -> Result<Arc<ServerConfig>> {
    let (tunnel_certs, tunnel_key) = parse_cert_and_key(info)?;
    let rustls_config =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(tunnel_certs, tunnel_key)?;
    Ok(Arc::new(rustls_config))
}
*/

/*
let our_cert = CertificateDer::pem_slice_iter(cert_chain_pem.as_bytes())
    .into_iter()
    .next()
    .context("missing certificate")??;
let (_, parsed_cert) = parse_x509_certificate(&our_cert)
    .map_err(|e| anyhow!("failed to parse certificate: {e}"))?;
println!("subject: {}", parsed_Send+cert.subject());
let san = parsed_cert.subject_alternative_name()?;
println!("san: {:?}", san);
let expiration = parsed_cert.validity().not_after.to_datetime();
println!("expiratoin: {:?}", expiration);
*/
