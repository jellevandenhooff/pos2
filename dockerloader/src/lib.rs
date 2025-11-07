pub const ENTRYPOINT_PATH: &str = "/data/dockerloader/entrypoint";
pub const ENTRYPOINT_ATTEMPT_PATH: &str = "/data/dockerloader/entrypoint-attempt";
pub const ENTRYPOINT_ATTEMPTING_PATH: &str = "/data/dockerloader/entrypoint-attempting";
pub const UPDATE_ATTEMPT_FILE: &str = "/data/dockerloader/update-attempt";
pub const RESTART_EXIT_CODE: i32 = 42;

/// Check if running in CLI mode (indicated by "cli" as first argument)
/// Returns the arguments after "cli" if in CLI mode, None otherwise
pub fn is_cli_mode() -> Option<Vec<String>> {
    if std::env::args().nth(1).as_deref() == Some("cli") {
        Some(std::env::args().skip(2).collect())
    } else {
        None
    }
}

use anyhow::{Result, bail, Context};
use flate2::read::GzDecoder;
use futures::StreamExt;
use oci_client::{Reference, manifest::OciImageManifest};
use std::path::{Path, PathBuf};
use tar::Archive;
use tokio::io::AsyncWriteExt;

// TODO: fsync???
fn uuid() -> String {
    uuid::Uuid::new_v4().hyphenated().to_string()
}

async fn clean_tmp_in_dir(path: &std::path::Path) -> Result<()> {
    // Rename tmp-* to removing-*
    let mut iter = tokio::fs::read_dir(path).await?;
    while let Some(entry) = iter.next_entry().await? {
        let Ok(old_name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(trimmed) = old_name.strip_prefix("tmp-") else {
            continue;
        };
        let new_name = format!("removing-{}", trimmed);

        tracing::info!("marking tmp file for removal: {}", old_name);
        let old_path = path.join(&old_name);
        let new_path = path.join(&new_name);
        tokio::fs::rename(&old_path, &new_path).await?;
    }

    // Remove all removing-* files/dirs
    let mut iter = tokio::fs::read_dir(path).await?;
    while let Some(entry) = iter.next_entry().await? {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.starts_with("removing-") {
            continue;
        };
        let Ok(info) = entry.metadata().await else {
            continue;
        };
        tracing::info!("removing tmp item: {}", name);
        if info.is_dir() {
            tokio::fs::remove_dir_all(entry.path()).await?;
        } else {
            tokio::fs::remove_file(entry.path()).await?;
        }
    }
    Ok(())
}

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

pub async fn pull_manifest_maybe_blob(
    oci_client: &oci_client::Client,
    reference: &Reference,
) -> Result<(oci_client::manifest::OciManifest, String)> {
    let auth = oci_client::secrets::RegistryAuth::Anonymous;
    match oci_client.pull_manifest(&reference, &auth).await {
        Ok((manifest, sha)) => Ok((manifest, sha)),
        Err(err) => {
            let Some(digest) = reference.digest() else {
                bail!(err);
            };
            let mut bytes: Vec<u8> = vec![];
            oci_client.pull_blob(&reference, digest, &mut bytes).await?;
            let manifest: oci_client::manifest::OciManifest = serde_json::from_slice(&bytes)?;
            Ok((manifest, digest.to_string()))
        }
    }
}

pub async fn download_manifest(
    oci_client: &oci_client::Client,
    reference: &Reference,
) -> Result<(Reference, OciImageManifest, String)> {
    let (manifest, sha) = pull_manifest_maybe_blob(oci_client, &reference).await?;

    match manifest {
        oci_client::manifest::OciManifest::Image(image) => {
            return Ok((reference.clone(), image, sha));
        }
        oci_client::manifest::OciManifest::ImageIndex(index) => {
            let Some(digest) = oci_client::client::current_platform_resolver(&index.manifests)
            else {
                bail!("did not find image in manifest");
            };

            let new_reference = reference.clone_with_digest(digest.clone());
            let (manifest, sha) = pull_manifest_maybe_blob(oci_client, &new_reference).await?;

            let oci_client::manifest::OciManifest::Image(image) = manifest else {
                bail!("did not get image from entry?");
            };
            return Ok((new_reference, image, sha));
        }
    };
}

pub async fn download_image(client: &oci_client::Client, reference: &Reference) -> Result<String> {
    let images_dir = PathBuf::from("/data/dockerloader/storage/v1/images");
    tokio::fs::create_dir_all(&images_dir).await?;

    let tmp_path = images_dir.join(format!("tmp-{}", uuid()));

    let (reference, manifest, sha) = download_manifest(client, reference).await?;

    let manifest_json = serde_json::to_string_pretty(&manifest)?;
    tokio::fs::write(&tmp_path, &manifest_json).await?;

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

pub async fn cleanup_storage(keep_shas: &[&str]) -> Result<()> {
    use std::collections::HashSet;

    let blobs_dir = PathBuf::from("/data/dockerloader/storage/v1/blobs");
    let images_dir = PathBuf::from("/data/dockerloader/storage/v1/images");
    let extracted_dir = PathBuf::from("/data/dockerloader/storage/v1/extracted");

    // Clean tmp files in all directories
    for dir in [&blobs_dir, &images_dir, &extracted_dir] {
        if let Ok(true) = tokio::fs::try_exists(dir).await {
            if let Err(e) = clean_tmp_in_dir(dir).await {
                tracing::warn!("failed to clean tmp in {:?}: {}", dir, e);
            }
        }
    }

    let keep_shas: HashSet<&str> = keep_shas.iter().copied().collect();

    // Collect all blobs referenced by kept images
    let mut referenced_blobs = HashSet::new();
    for sha in &keep_shas {
        let manifest_path = images_dir.join(sha);
        let Ok(manifest_json) = tokio::fs::read_to_string(&manifest_path).await else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<OciImageManifest>(&manifest_json) else {
            continue;
        };
        referenced_blobs.insert(manifest.config.digest);
        for layer in manifest.layers {
            referenced_blobs.insert(layer.digest);
        }
    }

    // Delete old extracted directories
    if let Ok(mut iter) = tokio::fs::read_dir(&extracted_dir).await {
        while let Some(entry) = iter.next_entry().await.ok().flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !keep_shas.contains(name_str.as_ref()) {
                tracing::info!("removing old extracted dir: {}", name_str);
                let _ = tokio::fs::remove_dir_all(entry.path()).await;
            }
        }
    }

    // Delete old image manifests
    if let Ok(mut iter) = tokio::fs::read_dir(&images_dir).await {
        while let Some(entry) = iter.next_entry().await.ok().flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !keep_shas.contains(name_str.as_ref()) {
                tracing::info!("removing old image manifest: {}", name_str);
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }

    // Delete unreferenced blobs
    if let Ok(mut iter) = tokio::fs::read_dir(&blobs_dir).await {
        while let Some(entry) = iter.next_entry().await.ok().flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !referenced_blobs.contains(name_str.as_ref()) {
                tracing::info!("removing unreferenced blob: {}", name_str);
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }

    Ok(())
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

pub async fn was_update_attempted(sha: &str) -> bool {
    if let Ok(attempted_sha) = tokio::fs::read_to_string(UPDATE_ATTEMPT_FILE).await {
        attempted_sha.trim() == sha
    } else {
        false
    }
}

pub async fn apply_image_env_vars(sha: &str) -> Result<()> {
    let path = format!("/data/dockerloader/storage/v1/extracted/{sha}/.env");
    let env_file = tokio::fs::read_to_string(path).await?;
    let lines = env_file.lines();
    for line in lines {
        if let Some((key, value)) = line.split_once('=') {
            unsafe {
                std::env::set_var(key, value);
            }
        }
    }
    Ok(())
}

pub async fn get_running_image_sha() -> Result<String> {
    let exe_path = std::env::current_exe().context("failed to get current executable path")?;

    // Extract sha from path like /data/dockerloader/storage/v1/extracted/{sha}/entrypoint
    let path_str = exe_path.to_string_lossy();
    let parts: Vec<&str> = path_str.split('/').collect();

    // Find the part after "extracted"
    for (i, part) in parts.iter().enumerate() {
        if *part == "extracted" && i + 1 < parts.len() {
            return Ok(parts[i + 1].to_string());
        }
    }

    bail!("could not find sha in executable path: {}", path_str)
}

pub async fn check_for_update(reference: &str, current_sha: &str) -> Result<()> {
    tracing::info!("checking for updates for {}", reference);

    let oci_client = create_oci_client();
    let reference: oci_client::Reference = reference.try_into()?;

    let (reference, _manifest, new_sha) = download_manifest(&oci_client, &reference).await?;

    tracing::info!("current SHA: {}, latest SHA: {}", current_sha, new_sha);

    if current_sha == new_sha {
        tracing::info!("already running the latest version");
        return Ok(());
    }

    if was_update_attempted(&new_sha).await {
        tracing::warn!("skipping update to {} - previous attempt failed", new_sha);
        return Ok(());
    }

    tracing::info!(
        "update available, downloading version {} from ref {}",
        new_sha,
        reference
    );

    download_image(&oci_client, &reference).await?;

    let extracted_path = extract_image(&new_sha).await?;
    let new_entrypoint = extracted_path.join("entrypoint");

    tokio::fs::write(UPDATE_ATTEMPT_FILE, &new_sha)
        .await
        .context("failed to write update-attempt marker")?;

    // Create entrypoint-attempt symlink for supervisor to pick up
    let tmp_symlink = format!("{}.tmp", ENTRYPOINT_ATTEMPT_PATH);
    if let Ok(true) = tokio::fs::try_exists(&tmp_symlink).await {
        tokio::fs::remove_file(&tmp_symlink).await?;
    }
    tokio::fs::symlink(&new_entrypoint, &tmp_symlink)
        .await
        .context("failed to create temporary symlink")?;
    tokio::fs::rename(&tmp_symlink, ENTRYPOINT_ATTEMPT_PATH)
        .await
        .context("failed to create entrypoint-attempt symlink")?;

    tracing::info!("created entrypoint-attempt, exiting to trigger restart");
    std::process::exit(RESTART_EXIT_CODE);
}

pub async fn init_dockerloaded() -> Result<()> {
    let current_sha = get_running_image_sha().await?;

    apply_image_env_vars(&current_sha).await?;

    // skip CA certificate setup in CLI mode
    if is_cli_mode().is_none() {
        // copy bundled CA certificates to root filesystem
        let bundled_ca_certs = format!("/data/dockerloader/storage/v1/extracted/{}/etc/ssl/certs/ca-certificates.crt", current_sha);
        let root_ca_certs = "/etc/ssl/certs/ca-certificates.crt";

        if let Ok(true) = tokio::fs::try_exists(&bundled_ca_certs).await {
            tokio::fs::create_dir_all("/etc/ssl/certs").await?;
            tokio::fs::copy(&bundled_ca_certs, root_ca_certs).await?;
            tracing::info!("copied CA certificates to {}", root_ca_certs);
        }
    }

    let in_trial_mode = std::env::var("DOCKERLOADER_TRIAL").is_ok();

    tracing::info!("dockerloaded started (trial mode: {})", in_trial_mode);

    Ok(())
}

pub async fn mark_ready() -> Result<()> {
    let current_sha = get_running_image_sha().await?;
    let in_trial_mode = std::env::var("DOCKERLOADER_TRIAL").is_ok();

    if in_trial_mode {
        // Check if already committed
        if tokio::fs::try_exists(ENTRYPOINT_ATTEMPTING_PATH).await? {
            tracing::info!("marking ready by renaming entrypoint-attempting to entrypoint");
            tokio::fs::rename(ENTRYPOINT_ATTEMPTING_PATH, ENTRYPOINT_PATH)
                .await
                .context("failed to rename entrypoint-attempting to entrypoint")?;

            let _ = tokio::fs::remove_file(UPDATE_ATTEMPT_FILE).await;

            tracing::info!("trial successful, committed to version {}", current_sha);
        }
    }

    if let Err(e) = cleanup_storage(&[&current_sha]).await {
        tracing::warn!("cleanup failed: {}", e);
    }

    let reference = std::env::var("DOCKERLOADER_TARGET")?;
    check_for_update(&reference, &current_sha).await?;

    Ok(())
}

pub async fn start_update_loop() -> Result<tokio::task::JoinHandle<()>> {
    let interval_secs = std::env::var("DOCKERLOADER_UPDATE_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(300);

    let reference = std::env::var("DOCKERLOADER_TARGET")
        .context("DOCKERLOADER_TARGET environment variable not set")?;

    tracing::info!(
        "starting update loop: checking {} every {} seconds",
        reference,
        interval_secs
    );

    let handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Skip the first tick which fires immediately
        interval.tick().await;

        loop {
            interval.tick().await;

            tracing::debug!("background update check triggered");

            match get_running_image_sha().await {
                Ok(current) => {
                    if let Err(e) = check_for_update(&reference, &current).await {
                        tracing::warn!("background update check failed: {}", e);
                    }
                }
                Err(e) => {
                    tracing::warn!("failed to get current sha for update check: {}", e);
                }
            }
        }
    });

    Ok(handle)
}
