pub const ENTRYPOINT_PATH: &str = "/data/dockerloader/entrypoint";

use anyhow::{Result, bail};
use flate2::read::GzDecoder;
use futures::StreamExt;
use oci_client::{Reference, manifest::OciImageManifest};
use std::path::{Path, PathBuf};
use tar::Archive;
use tokio::io::AsyncWriteExt;

pub fn execve_into(path: &Path, env: &Vec<(String, String)>) -> Result<std::convert::Infallible> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path_cstr = CString::new(path.as_os_str().as_bytes())?;
    let args = vec![path_cstr.clone()];
    let env: Vec<CString> = env
        .iter()
        .map(|(key, value)| CString::new(format!("{}={}", key, value)))
        .collect::<Result<_, _>>()?;
    Ok(nix::unistd::execve(&path_cstr, &args, &env)?)
}

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
