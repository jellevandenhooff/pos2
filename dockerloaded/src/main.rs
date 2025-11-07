use std::sync::{Arc, atomic::AtomicBool};

use anyhow::{Context, Result};

const UPDATE_ATTEMPT_FILE: &str = "/data/dockerloader/update-attempt";

async fn was_update_attempted(sha: &str) -> bool {
    if let Ok(attempted_sha) = tokio::fs::read_to_string(UPDATE_ATTEMPT_FILE).await {
        attempted_sha.trim() == sha
    } else {
        false
    }
}

async fn apply_image_env_vars(sha: &str) -> Result<()> {
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

async fn get_running_image_sha() -> Result<String> {
    // Get the current executable path
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

    anyhow::bail!("could not find sha in executable path: {}", path_str)
}

async fn check_for_update(
    reference: &str,
    current_sha: &str,
    original_env: &[(String, String)],
) -> Result<()> {
    tracing::info!("Checking for updates for {}", reference);

    let oci_client = dockerloader::create_oci_client();
    let reference: oci_client::Reference = reference.try_into()?;

    // Download the manifest to get the latest sha
    let (reference, _manifest, new_sha) =
        dockerloader::download_manifest(&oci_client, &reference).await?;

    tracing::info!("Current SHA: {}, Latest SHA: {}", current_sha, new_sha);

    // Check if there's an update
    if current_sha == new_sha {
        tracing::info!("already running the latest version");
        return Ok(());
    }

    if was_update_attempted(&new_sha).await {
        tracing::warn!("skipping update to {} - previous attempt failed", new_sha);
        // Continue running current version
        return Ok(());
    }

    tracing::info!(
        "update available, downloading version {} from ref {}",
        new_sha,
        reference
    );

    // Download and extract the new image
    dockerloader::download_image(&oci_client, &reference).await?;

    let extracted_path = dockerloader::extract_image(&new_sha).await?;
    let new_entrypoint = extracted_path.join("entrypoint");

    // Mark that we're attempting this update
    // If the trial fails, this file will remain and prevent retrying the same SHA
    tokio::fs::write(UPDATE_ATTEMPT_FILE, &new_sha)
        .await
        .context("failed to write update-attempt marker")?;

    tracing::info!("execve into {:?} for trial", new_entrypoint);

    // Build environment with DOCKERLOADER_TRIAL=1 added to original env
    let mut trial_env = original_env.to_vec();
    trial_env.push(("DOCKERLOADER_TRIAL".to_string(), "1".to_string()));

    // Execve into new binary with trial mode enabled
    dockerloader::execve_into(&new_entrypoint, &trial_env)?;

    Ok(())
}

// TODO: run continuously
// TODO: move all (?) code into lib
// TODO: extract into wasi3experiment
// TODO: make the install script somehow wait for the initial download to finish (for exec cli -- maybe dockerloader itself can be the entrypoint and wait?) -- and what happens with the trial balloon?
// TODO: only copy (some) of the files into the loaded docker container?
// TODO: maybe move the exec/restart to the outer dockerloader -- then it can do the timer??? and include the env??

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // save original environment for if we exec into new
    let original_env: Vec<_> = std::env::vars().collect();
    let current_sha = get_running_image_sha().await?;

    apply_image_env_vars(&current_sha).await?;

    let in_trial_mode = std::env::var("DOCKERLOADER_TRIAL").is_ok();
    let trial_succeeded = Arc::new(AtomicBool::new(false));

    if in_trial_mode {
        let timeout_duration = if std::env::var("TIMEOUT_INIT").is_ok() {
            std::time::Duration::from_millis(500)
        } else {
            std::time::Duration::from_secs(10)
        };

        let trial_succeeded = trial_succeeded.clone();

        std::thread::spawn(move || {
            std::thread::sleep(timeout_duration);
            eprintln!(
                "DOCKERLOADER_TRIAL timeout hit after {:#?}, aborting",
                timeout_duration
            );
            if !trial_succeeded.load(std::sync::atomic::Ordering::SeqCst) {
                std::process::abort();
            }
        });
    }

    tracing::info!("dockerloaded started (trial mode: {})", in_trial_mode);

    // Check for intentional test failure
    if std::env::var("FAIL_INIT").is_ok_and(|v| v == "1") {
        anyhow::bail!("intentional failure for testing (FAIL_INIT=1)");
    }

    // Check for intentional test timeout
    if std::env::var("TIMEOUT_INIT").is_ok_and(|v| v == "1") {
        tracing::info!("TIMEOUT_INIT=1, sleeping for 60 seconds");
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }

    // If in trial mode, update symlink to commit to this version
    if in_trial_mode {
        let current_entrypoint =
            format!("/data/dockerloader/storage/v1/extracted/{current_sha}/entrypoint");
        // let current_entrypoint =
        // std::env::current_exe().context("failed to get current executable")?;
        let entrypoint_path = dockerloader::ENTRYPOINT_PATH;

        // Create new symlink at temporary location
        let tmp_symlink = format!("{}.tmp", entrypoint_path);
        if let Ok(true) = tokio::fs::try_exists(&tmp_symlink).await {
            tokio::fs::remove_file(&tmp_symlink).await?;
        }
        tokio::fs::symlink(&current_entrypoint, &tmp_symlink)
            .await
            .context("failed to create temporary symlink")?;

        // Replace the old symlink
        tokio::fs::rename(&tmp_symlink, entrypoint_path)
            .await
            .context("failed to update entrypoint symlink")?;

        trial_succeeded.store(true, std::sync::atomic::Ordering::SeqCst);

        // Delete the update-attempt marker - we succeeded!
        let _ = tokio::fs::remove_file(UPDATE_ATTEMPT_FILE).await;

        tracing::info!("trial successful, committed to version {}", current_sha);
    }

    // Clean up old images and blobs
    if let Err(e) = dockerloader::cleanup_storage(&[&current_sha]).await {
        tracing::warn!("cleanup failed: {}", e);
    }

    let reference = std::env::var("DOCKERLOADER_TARGET")?;
    check_for_update(&reference, &current_sha, &original_env).await?;

    // Run the actual application logic
    let version = std::env::var("VERSION").unwrap_or_else(|_| "unknown".to_string());
    println!("Hello, world! Version: {}", version);

    Ok(())
}
