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
    // Read the manifest to get config digest
    let manifest_path = format!("/data/dockerloader/storage/v1/images/{}", sha);
    let manifest_json = tokio::fs::read_to_string(&manifest_path).await?;
    let manifest: oci_client::manifest::OciImageManifest = serde_json::from_str(&manifest_json)?;

    // Read the config blob
    let config_path = format!(
        "/data/dockerloader/storage/v1/blobs/{}",
        manifest.config.digest
    );
    let config_json = tokio::fs::read_to_string(&config_path).await?;

    // Parse as oci_spec ImageConfiguration
    let config: oci_spec::image::ImageConfiguration = serde_json::from_str(&config_json)?;

    // Apply environment variables from the config, overriding dockerloader's env
    if let Some(env_vars) = config.config().as_ref().and_then(|c| c.env().as_ref()) {
        for env_var in env_vars {
            if let Some(pos) = env_var.find('=') {
                let key = &env_var[..pos];
                let value = &env_var[pos + 1..];
                unsafe {
                    std::env::set_var(key, value);
                }
                tracing::debug!("Set env var from config: {}={}", key, value);
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

async fn check_for_update(reference: &str, current_sha: &str) -> Result<Option<String>> {
    tracing::info!("Checking for updates for {}", reference);

    let oci_client = dockerloader::create_oci_client();
    let reference: oci_client::Reference = reference.try_into()?;

    // Download the manifest to get the latest sha
    let (_reference, _manifest, latest_sha) =
        dockerloader::download_manifest(&oci_client, &reference).await?;

    tracing::info!("Current SHA: {}, Latest SHA: {}", current_sha, latest_sha);

    // Check if there's an update
    if current_sha == latest_sha {
        tracing::info!("Already running the latest version");
        Ok(None)
    } else {
        tracing::info!("New version available: {}", latest_sha);
        Ok(Some(latest_sha))
    }
}

async fn perform_update(
    reference: &str,
    new_sha: &str,
    original_env: &[(String, String)],
) -> Result<std::convert::Infallible> {
    tracing::info!("performing update to {}", new_sha);

    let oci_client = dockerloader::create_oci_client();
    let reference: oci_client::Reference = reference.try_into()?;

    // Download and extract the new image
    let downloaded_sha = dockerloader::download_image(&oci_client, &reference).await?;
    if downloaded_sha != new_sha {
        // TODO: decide if this should be an error or if it can never happen
        tracing::warn!(
            "downloaded sha {} doesn't match expected {}",
            downloaded_sha,
            new_sha
        );
    }

    let extracted_path = dockerloader::extract_image(&downloaded_sha).await?;
    let new_entrypoint = extracted_path.join("entrypoint");

    // Mark that we're attempting this update
    // If the trial fails, this file will remain and prevent retrying the same SHA
    tokio::fs::write(UPDATE_ATTEMPT_FILE, &downloaded_sha)
        .await
        .context("failed to write update-attempt marker")?;

    tracing::info!("execve into {} for trial", new_entrypoint);

    // Build environment with DOCKERLOADER_TRIAL=1 added to original env
    let mut trial_env = original_env.to_vec();
    trial_env.push(("DOCKERLOADER_TRIAL".to_string(), "1".to_string()));

    // Execve into new binary with trial mode enabled
    dockerloader::execve_into(&new_entrypoint, &trial_env)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // Save original environment before we modify it
    let original_env: Vec<(String, String)> = std::env::vars().collect();

    // Get current image SHA once
    let current_sha = get_running_image_sha().await?;

    // Apply environment variables from the image config
    apply_image_env_vars(&current_sha).await?;

    let in_trial_mode = std::env::var("DOCKERLOADER_TRIAL").is_ok();
    if in_trial_mode {
        let timeout_duration = if std::env::var("TIMEOUT_INIT").is_ok() {
            std::time::Duration::from_millis(500)
        } else {
            std::time::Duration::from_secs(10)
        };

        tokio::spawn(async move {
            tokio::time::sleep(timeout_duration).await;
            eprintln!(
                "DOCKERLOADER_TRIAL timeout hit after {:#?}, aborting",
                timeout_duration
            );
            std::process::abort();
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
            std::env::current_exe().context("failed to get current executable")?;
        let entrypoint_path = dockerloader::ENTRYPOINT_PATH;
        let tmp_symlink = format!("{}.tmp", entrypoint_path);

        // Create new symlink at temporary location
        tokio::fs::symlink(&current_entrypoint, &tmp_symlink)
            .await
            .context("failed to create temporary symlink")?;

        // Atomically replace the old symlink
        tokio::fs::rename(&tmp_symlink, entrypoint_path)
            .await
            .context("failed to update entrypoint symlink")?;

        // Delete the update-attempt marker - we succeeded!
        let _ = tokio::fs::remove_file(UPDATE_ATTEMPT_FILE).await;

        tracing::info!("trial successful, committed to version {}", current_sha);
    }

    // TODO: get from environment?
    let reference = "host.docker.internal:5050/dockerloaded:testing";

    // Check for updates
    match check_for_update(reference, &current_sha).await {
        Ok(Some(new_sha)) => {
            // Check if this update was already attempted and failed
            if was_update_attempted(&new_sha).await {
                tracing::warn!("skipping update to {} - previous attempt failed", new_sha);
                // Continue running current version
            } else {
                tracing::info!("update available, downloading version {}", new_sha);
                perform_update(reference, &new_sha, &original_env).await?;
            }
        }
        Ok(None) => {
            tracing::info!("already running the latest version");
        }
        Err(e) => {
            tracing::error!("failed to check for updates: {}", e);
        }
    }

    // Run the actual application logic
    let version = std::env::var("VERSION").unwrap_or_else(|_| "unknown".to_string());
    println!("Hello, world! Version: {}", version);

    Ok(())
}
