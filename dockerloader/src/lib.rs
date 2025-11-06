pub const ENTRYPOINT_PATH: &str = "/data/dockerloader/entrypoint";

use anyhow::{Result, bail};
use flate2::read::GzDecoder;
use futures::StreamExt;
use std::path::{Path, PathBuf};
use tar::Archive;
use tokio::io::AsyncWriteExt;

use oci_client::{Reference, manifest::OciImageManifest};

pub fn execve_into(path: &Path, env: &[(String, String)]) -> Result<std::convert::Infallible> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path_cstr = CString::new(path.as_os_str().as_bytes())?;
    let args = vec![path_cstr.clone()];
    let env: Vec<CString> = env
        .iter()
        .map(|(key, value)| CString::new(format!("{}={}", key, value)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(nix::unistd::execve(&path_cstr, &args, &env)?)
}

// TODO: fsync???
fn uuid() -> String {
    uuid::Uuid::new_v4().hyphenated().to_string()
}

async fn clean_tmp_in_dir(path: &std::path::Path) -> Result<()> {
    let mut iter = tokio::fs::read_dir(path).await?;
    while let Some(entry) = iter.next_entry().await? {
        let Ok(old_name) = entry.file_name().into_string() else {
            // TODO: complain about name?
            continue;
        };
        let Some(trimmed) = old_name.strip_prefix("tmp-") else {
            continue;
        };
        let new_name = format!("removing-{}", trimmed);

        let old_path = path.join(&old_name);
        let new_path = path.join(&new_name);

        // tolerate errors?
        tokio::fs::rename(&old_path, &new_path).await?;
    }

    let mut iter = tokio::fs::read_dir(path).await?;
    while let Some(entry) = iter.next_entry().await? {
        let Ok(info) = entry.metadata().await else {
            // TODO: log err instead?
            continue;
        };
        if info.is_dir() {
            // tolerate errors?
            tokio::fs::remove_dir_all(entry.path()).await?;
        } else {
            tokio::fs::remove_file(entry.path()).await?;
        }
    }
    Ok(())
}

// async fn clean_unused_images() -> Result<()> {}

async fn download_layer(
    client: &oci_client::Client,
    image: &Reference,
    descriptor: &str,
) -> Result<PathBuf> {
    let blobs_dir = PathBuf::from("/data/dockerloader/storage/v1/blobs");
    tokio::fs::create_dir_all(&blobs_dir).await?;

    let final_path = blobs_dir.join(descriptor);
    if let Ok(true) = tokio::fs::try_exists(&final_path).await {
        return Ok(final_path);
    }
    let tmp_path = blobs_dir.join(format!("tmp-{}", uuid()));

    // mkdir all
    // TODO: progress tracking somehow/somewhere?
    let mut stream = client.pull_blob_stream(&image, descriptor).await?;

    let mut file = tokio::fs::File::create(&tmp_path).await?;

    loop {
        let Some(result) = stream.stream.next().await else {
            break;
        };
        let chunk = result?;
        file.write(&chunk).await?;
    }

    tokio::fs::rename(&tmp_path, &final_path).await?;

    Ok(final_path)
}

pub async fn download_manifest(
    oci_client: &oci_client::Client,
    reference: &Reference,
) -> Result<(Reference, OciImageManifest, String)> {
    let auth = oci_client::secrets::RegistryAuth::Anonymous;
    let (manifest, sha) = oci_client.pull_manifest(&reference, &auth).await?;
    println!("manifest: {:?}", manifest);
    println!("sha: {:?}", sha);

    let current_architecture = "arm64"; // XXX: find this out how?

    let (reference, manifest, sha) = match manifest {
        oci_client::manifest::OciManifest::Image(image) => (reference.clone(), image, sha),
        oci_client::manifest::OciManifest::ImageIndex(index) => {
            let Some(digest) = index
                .manifests
                .into_iter()
                .filter(|image| {
                    image
                        .platform
                        .as_ref()
                        .is_some_and(|platform| platform.architecture == current_architecture)
                })
                .next()
                .map(|entry| entry.digest)
            else {
                bail!("did not find image in manifest");
            };

            let new_reference = reference.clone_with_digest(digest.clone());
            match oci_client.pull_manifest(&new_reference, &auth).await {
                Ok((manifest, sha)) => match manifest {
                    oci_client::manifest::OciManifest::Image(image) => (new_reference, image, sha),
                    _ => bail!("did not get image from entry?"),
                },
                Err(err) => {
                    tracing::error!(
                        "got error fetching manifest {}; trying to fetch it as a blob",
                        err
                    );
                    let mut bytes: Vec<u8> = vec![];
                    oci_client
                        .pull_blob(&reference, digest.as_str(), &mut bytes)
                        .await?;
                    let manifest: oci_client::manifest::OciImageManifest =
                        serde_json::from_slice(&bytes)?;
                    let new_reference = reference.clone_with_digest(digest.clone());
                    let sha = digest;
                    (new_reference, manifest, sha)
                }
            }
        }
    };
    println!("manifest: {:?}", manifest);
    println!("sha: {:?}", sha);

    Ok((reference, manifest, sha))
}

pub async fn download_image(client: &oci_client::Client, reference: &Reference) -> Result<String> {
    let images_dir = PathBuf::from("/data/dockerloader/storage/v1/images");
    tokio::fs::create_dir_all(&images_dir).await?;

    let tmp_path = images_dir.join(format!("tmp-{}", uuid()));

    let (reference, manifest, sha) = download_manifest(client, reference).await?;

    let manifest_json = serde_json::to_string_pretty(&manifest)?;
    tokio::fs::write(&tmp_path, &manifest_json).await?;

    // Download the config blob
    download_layer(client, &reference, &manifest.config.digest).await?;

    for layer in manifest.layers {
        download_layer(client, &reference, &layer.digest).await?;
    }

    let final_path = images_dir.join(&sha);
    tokio::fs::rename(&tmp_path, &final_path).await?;

    Ok(sha)
}

pub async fn extract_image(sha: &str) -> Result<PathBuf> {
    let extracted_dir = PathBuf::from("/data/dockerloader/storage/v1/extracted");
    let final_path = extracted_dir.join(sha);
    if let Ok(true) = tokio::fs::try_exists(&final_path).await {
        return Ok(final_path);
    }

    let tmp_path = extracted_dir.join(format!("tmp-{}", uuid()));

    tokio::fs::create_dir_all(&tmp_path).await?;

    let images_dir = PathBuf::from("/data/dockerloader/storage/v1/images");
    let manifest_path = images_dir.join(sha);
    let manifest_json = tokio::fs::read_to_string(&manifest_path).await?;
    let manifest: OciImageManifest = serde_json::from_str(&manifest_json)?;

    // TODO: this blocks task?
    let blobs_dir = PathBuf::from("/data/dockerloader/storage/v1/blobs");
    for layer in manifest.layers {
        let blob_path = blobs_dir.join(&layer.digest);
        let tar_gz = std::fs::File::open(&blob_path)?;
        let tar = GzDecoder::new(tar_gz);
        let mut archive = Archive::new(tar);
        archive.unpack(&tmp_path)?;
    }

    tokio::fs::rename(&tmp_path, &final_path).await?;

    Ok(final_path)
}

pub fn create_oci_client() -> oci_client::Client {
    let mut oci_config = oci_client::client::ClientConfig::default();
    oci_config.protocol =
        oci_client::client::ClientProtocol::HttpsExcept(vec!["host.docker.internal:5050".into()]);
    oci_client::client::Client::new(oci_config)
}

pub async fn download_entrypoint_initial(reference: &str) -> Result<()> {
    let oci_client = create_oci_client();
    // let auth = oci_client::secrets::RegistryAuth::Anonymous;
    let reference: oci_client::Reference = reference.try_into()?;

    let sha = download_image(&oci_client, &reference).await?;
    let extracted_path = extract_image(&sha).await?;

    // Ensure the parent directory exists
    if let Some(parent) = Path::new(ENTRYPOINT_PATH).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let entrypoint_source = extracted_path.join("entrypoint");
    tokio::fs::symlink(&entrypoint_source, ENTRYPOINT_PATH).await?;

    Ok(())
}

pub async fn try_upgrade(new_reference: &str) -> Result<()> {
    // download image
    // execve into it with a flag that says "trying"
    // if it works... it updates entrypoint
    // if it does not work... hopefully we restart (alternative: spawn a watchdog that does the updater that can abort)
    // for that: track that we are trying???
    Ok(())
}

enum Status {
    ReadyToTry,
    Trying,
    Succeeded,
    Failed,
}

struct State {
    current: String,
    pending: String,
}

// so... state? configuration? where?
// ok... to make this "real"-ish we want:
// - one (or more) concurrent installations
// - downloading of new images (and deleting them)
// - ability to switch between installations (with a healthcheck etc)

// operations:...
// - download a new version
// - try it, mark as success (or failure)
// - remove old (or new) version
// - (re-)start
// storage ideas:
// if missing... bootstrap versions configured somewhere?
// v1/layers/<sha>
// v1/images/<image-id>/<layer-sha> -> link to layers/
// v1/extracted/<image-id>
// v1/tmp/... -> v1/tmp/...-removing | dst
// loader/entrypoint -> link to binary # extracted/<id>/binary (or to self in container?)
// loader/attempt -> link to binary # extracted/<id>

// TODO: for bootstrap, don't have cleanup (in the same way?)

// start (execve?) /data/entrypoint
//
//

/*
let (manifest, sha) = oci_client.pull_manifest(&reference, &auth).await?;
println!("manifest: {:?}", manifest);
println!("sha: {:?}", sha);

let current_architecture = "arm64"; // XXX: find this out how?

let (reference, image, sha) = match manifest {
    oci_client::manifest::OciManifest::Image(image) => (reference, image, sha),
    oci_client::manifest::OciManifest::ImageIndex(index) => {
        let Some(digest) = index
            .manifests
            .into_iter()
            .filter(|image| {
                image
                    .platform
                    .as_ref()
                    .is_some_and(|platform| platform.architecture == current_architecture)
            })
            .next()
            .map(|entry| entry.digest)
        else {
            bail!("did not find image in manifest");
        };

        let new_reference = reference.clone_with_digest(digest);
        let (manifest, sha) = oci_client.pull_manifest(&new_reference, &auth).await?;
        match manifest {
            oci_client::manifest::OciManifest::Image(image) => (new_reference, image, sha),
            _ => bail!("did not get image from entry?"),
        }
    }
};

println!("manifest: {:?}", image);
println!("sha: {:?}", sha);

for layer in &image.layers {
    let path = format!("/data/{}", layer.digest);
    if let Ok(true) = tokio::fs::try_exists(&path).await {
        continue;
    }

    // TODO: prevent partial writes or verify later
    let mut stream = oci_client.pull_blob_stream(&reference, &layer).await?;
    let mut file = tokio::fs::File::create(&path).await?;

    loop {
        let Some(result) = stream.stream.next().await else {
            break;
        };
        let chunk = result?;
        file.write(&chunk).await?;
    }
}

if let Ok(true) = tokio::fs::try_exists("/data/extracted").await {
    // TODO: maybe also have to set permissions here?
    // tokio::fs::remove_dir_all("/data/extracted").await?;
} else {
    tokio::fs::create_dir("/data/extracted").await?;

    // TODO: this blocks task?
    for layer in image.layers {
        let path = format!("/data/{}", layer.digest);
        let tar_gz = std::fs::File::open(path)?;
        let tar = GzDecoder::new(tar_gz);
        let mut archive = Archive::new(tar);
        archive.unpack("/data/extracted")?;
    }
}
*/

// TODO:
// ssl files?
// timezone files?
// env vars and junk from docker file?

// move this into a statically linked binary??
/*
let mut child = tokio::process::Command::new("/data/extracted/app")
    // TODO: maybe patch rpath instead? (but eh)
    .env(
        "LD_LIBRARY_PATH",
        "/data/extracted/lib/aarch64-linux-gnu:/data/extracted/usr/lib/aarch64-linux-gnu",
    )
    .spawn()?;
child.wait().await?;
*/

// main:
// - signal handler... something clever? tbd
//
// loop {
// - if loader/attempting exists, rename to loader/failed
// - if loader/attempt exists, rename to loader/attempting, start loader/attempting
// -   what about a healthcheck or watchdog timer???
// - otherwise, if loader/current exists, start that
// - if none exist, use downloader with information from image to download layers, images, and extract to extracted/
// }
// then the actual binary can do whatever it wants. no api necessary (???)
// hmmmm.... seems pretty complicated... maybe don't bother caching layers?
// what's the difference between phase1 and phase2? at some point we need a binary that can run without anything in the root, and then that thing can prepare the rest. maybe xit can just be part of the image?
/*
async fn download_image(image: String) {}
async fn try_image(image: String) {}
async fn mark_attempt_successful(image: String) {}
async fn mark_attempt_failed(image: String) {}
async fn status() -> String {
    "OK".into()
}

async fn remove_unused_layers() {}
*/

/*
install(
    std::path::Path::new("/data/extracted"),
    std::path::Path::new("/"),
)
.await?;

let mut child = tokio::process::Command::new("/bin/bash").spawn()?;
child.wait().await?;

uninstall(
    std::path::Path::new("/data/extracted"),
    std::path::Path::new("/"),
)
.await?;
*/

/*
async fn install(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    // TODO: permissions? make things read-only?
    // TODO: what about /tmp? why are we linking in empty directories?
    // TODO: maybe actually copy instead of link? (how bad can it be?) (and then restore to scratch-like?) (it would be slower but less weird?)

    println!("installing from {:?} to {:?}", src, dst);

    let mut paths = tokio::fs::read_dir(src).await?;
    while let Some(entry) = paths.next_entry().await? {
        let name = entry.file_name();
        let new_metadata = entry.metadata().await?;

        // TODO: handle errors
        let current_metadata = tokio::fs::symlink_metadata(dst.join(&name)).await.ok();

        if dst == std::path::Path::new("/") {
            if name == "proc" || name == "sys" || name == "dev" || name == "data" {
                continue;
            }
        }

        if new_metadata.is_dir() {
            if let Some(current_metadata) = current_metadata {
                if current_metadata.is_dir() {
                    Box::pin(install(&src.join(&name), &dst.join(&name))).await?;
                } else {
                    println!(
                        "ignoring dir {:?}; already exists and is not dir",
                        src.join(&name)
                    );
                }
            } else {
                println!("linking {:?} to {:?}", src.join(&name), dst.join(&name));
                tokio::fs::symlink(src.join(&name), dst.join(&name)).await?;
            }
        } else {
            if let Some(_) = current_metadata {
                println!("ignoring path {:?}; already exists", src.join(&name));
            } else {
                println!("linking {:?} to {:?}", src.join(&name), dst.join(&name));
                tokio::fs::symlink(src.join(&name), dst.join(&name)).await?;
            }
        }
    }
    Ok(())
}

async fn uninstall(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    // TODO: maybe actually do go into these paths?

    println!("uninstalling from {:?} to {:?}", src, dst);

    let mut paths = tokio::fs::read_dir(dst).await?;
    while let Some(entry) = paths.next_entry().await? {
        let name = entry.file_name();
        // let new_metadata = entry.metadata().await?;
        if dst == std::path::Path::new("/") {
            if name == "proc" || name == "sys" || name == "dev" || name == "data" {
                continue;
            }
        }

        // TODO: handle errors
        let path = dst.join(&name);
        let current_metadata = entry.metadata().await?;
        // println!("considering {:?}", path);
        // let Some(current_metadata) = tokio::fs::symlink_metadata(&path).await.ok() else {
        // println!("does not exist, skipping");
        // continue;
        // };

        if current_metadata.is_symlink() {
            let link_target = tokio::fs::read_link(&path).await?;
            // println!("is a link pointing to {:?}", link_target);
            if link_target.starts_with(src) {
                // == src.join(&name) {
                println!("removing link {:?}", &path);
                tokio::fs::remove_file(&path).await?;
            } else {
                println!(
                    "keeping link {:?}, target {:?} does not start with {:?}",
                    &path,
                    &link_target,
                    src,
                    // src.join(&name)
                );
            }
        } else if current_metadata.is_dir() {
            println!("{:?} is a dir, recursing", &path);
            Box::pin(uninstall(src /* &src.join(&name) */, &dst.join(&name))).await?;
        } else {
            println!("{:?} is something else, ignoring", path);
        }
    }
    Ok(())
}
*/

/*
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct UdsConnectInfo {
    peer_addr: Arc<tokio::net::unix::SocketAddr>,
    peer_cred: UCred,
}

impl connect_info::Connected<IncomingStream<'_, UnixListener>> for UdsConnectInfo {
    fn connect_info(stream: IncomingStream<'_, UnixListener>) -> Self {
        let peer_addr = stream.io().peer_addr().unwrap();
        let peer_cred = stream.io().peer_cred().unwrap();
        Self {
            peer_addr: Arc::new(peer_addr),
            peer_cred,
        }
    }
}
//
async fn handler(ConnectInfo(info): ConnectInfo<UdsConnectInfo>) -> &'static str {
    println!("new connection from `{info:?}`");

    "Hello, World!"
}
*/

/*
let path = PathBuf::from("/tmp/axum/helloworld");

let _ = tokio::fs::remove_file(&path).await;
tokio::fs::create_dir_all(path.parent().unwrap())
    .await
    .unwrap();

let uds = UnixListener::bind(path.clone()).unwrap();
tokio::spawn(async move {
    let app = Router::new()
        .route("/", get(handler))
        .into_make_service_with_connect_info::<UdsConnectInfo>();

    axum::serve(uds, app).await.unwrap();
});

let stream = TokioIo::new(UnixStream::connect(path).await.unwrap());
let (mut sender, conn) = hyper::client::conn::http1::handshake(stream).await.unwrap();
tokio::task::spawn(async move {
    if let Err(err) = conn.await {
        println!("Connection failed: {err:?}");
    }
});

let request = Request::builder()
    .method(Method::GET)
    // .uri("http://uri-doesnt-matter.com")
    .body(Body::empty())
    .unwrap();

let response = sender.send_request(request).await.unwrap();

assert_eq!(response.status(), StatusCode::OK);

let body = response.collect().await.unwrap().to_bytes();
let body = String::from_utf8(body.to_vec()).unwrap();
assert_eq!(body, "Hello, World!");
*/
