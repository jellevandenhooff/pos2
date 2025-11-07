use anyhow::{Context, Result};
use std::process::Command;

struct Docker {
    cwd: String,
}

impl Docker {
    fn new(cwd: &str) -> Self {
        Self {
            cwd: cwd.to_string(),
        }
    }

    fn run(&self, args: Vec<String>) -> Result<String> {
        let mut cmd = Command::new("docker");
        cmd.args(args);
        cmd.current_dir(&self.cwd);

        let output = cmd.output().context("failed to run docker command")?;

        if !output.status.success() {
            anyhow::bail!(
                "docker command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn build_dockerloader(&self) -> Result<()> {
        self.run(vec![
            "build".into(),
            "-t".into(),
            "dockerloader:testing".into(),
            "-f".into(),
            "Dockerfile.dockerloader".into(),
            ".".into(),
        ])?;
        Ok(())
    }

    fn build_and_push(&self, build_args: Vec<String>) -> Result<()> {
        let mut args = vec![
            "build".into(),
            "-t".into(),
            "localhost:5050/dockerloader-testapp:testing".into(),
            "-f".into(),
            "Dockerfile.dockerloaded".into(),
        ];

        for build_arg in build_args {
            args.push("--build-arg".into());
            args.push(build_arg);
        }

        args.push(".".into());
        self.run(args)?;
        self.run(vec!["push".into(), "localhost:5050/dockerloader-testapp:testing".into()])?;
        Ok(())
    }

    fn run_dockerloader(&self, data_dir: &str) -> Result<String> {
        self.run(vec![
            "run".into(),
            "--rm".into(),
            "-e".into(),
            "DOCKERLOADER_TRIAL_TIMEOUT_MS=500".into(),
            "-v".into(),
            format!("{}:/data", data_dir),
            "dockerloader:testing".into(),
        ])
    }

    fn run_detached(&self, data_dir: &str, name: &str, update_interval_secs: u64) -> Result<String> {
        let output = self.run(vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            name.into(),
            "-e".into(),
            "DOCKERLOADER_TRIAL_TIMEOUT_MS=500".into(),
            "-e".into(),
            format!("DOCKERLOADER_UPDATE_INTERVAL_SECS={}", update_interval_secs),
            "-v".into(),
            format!("{}:/data", data_dir),
            "dockerloader:testing".into(),
        ])?;
        Ok(output.trim().to_string())
    }

    fn logs(&self, container_id: &str) -> Result<String> {
        self.run(vec!["logs".into(), container_id.into()])
    }

    fn stop(&self, container_id: &str) -> Result<()> {
        self.run(vec!["stop".into(), container_id.into()])?;
        let _ = self.run(vec!["rm".into(), container_id.into()]);
        Ok(())
    }

    fn exec(&self, container_id: &str, command: Vec<String>) -> Result<String> {
        let mut args = vec!["exec".into(), container_id.into()];
        args.extend(command);
        self.run(args)
    }
}

fn read_symlink_target(data_dir: &str) -> Result<String> {
    let symlink_path = format!("{}/dockerloader/entrypoint", data_dir);
    let target = std::fs::read_link(&symlink_path)
        .with_context(|| format!("failed to read symlink at {}", symlink_path))?;
    Ok(target.to_string_lossy().to_string())
}

fn assert_symlink_contains(data_dir: &str, expected_sha: &str) -> Result<()> {
    let target = read_symlink_target(data_dir)?;
    if target.contains(expected_sha) {
        println!("Symlink points to {}", expected_sha);
        Ok(())
    } else {
        anyhow::bail!("symlink points to {}, expected {}", target, expected_sha)
    }
}

fn extract_sha(path: &str) -> Result<String> {
    // Extract sha256:... from path like /data/.../sha256:abc.../entrypoint
    for part in path.split('/') {
        if part.starts_with("sha256:") {
            return Ok(part.to_string());
        }
    }
    anyhow::bail!("could not find sha256: in path: {}", path)
}

fn count_files_in_dir(dir: &str) -> Result<usize> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(0);
    };
    Ok(entries.count())
}

#[test]
#[serial_test::serial]
fn test_dockerloader_update_flow() -> Result<()> {
    // CARGO_MANIFEST_DIR points to dockerloader/, we need the workspace root
    let workspace_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_str()
        .unwrap();
    let data_dir = format!("{}/dockerloader/data", workspace_dir);

    let _ = std::fs::remove_dir_all(&data_dir);

    let docker = Docker::new(workspace_dir);

    println!("Building dockerloader:testing");
    docker.build_dockerloader()?;

    println!("Building and pushing version 1");
    docker.build_and_push(vec!["VERSION=1.0".into()])?;

    println!("First run - should download version 1");
    let output = docker.run_dockerloader(&data_dir)?;
    assert!(output.contains("Version: 1.0"));

    println!("Building and pushing version 2");
    docker.build_and_push(vec!["VERSION=2.0".into()])?;

    println!("Second run - should detect update and download version 2");
    let output = docker.run_dockerloader(&data_dir)?;
    assert!(output.contains("Version: 2.0"));
    assert!(output.contains("trial mode: true"));
    assert!(output.contains("update available"));
    assert!(output.contains("trial successful"));

    println!("Third run - should run version 2 without updating");
    let output = docker.run_dockerloader(&data_dir)?;
    assert!(output.contains("Version: 2.0"));
    assert!(!output.contains("trial mode: true"));
    assert!(output.contains("already running the latest version"));

    let v2_target = read_symlink_target(&data_dir)?;
    println!("V2 symlink target: {}", v2_target);

    // Verify cleanup happened - should only have v2 now, v1 should be cleaned up
    println!("Verifying cleanup removed old v1 files");
    let extracted_dir = format!("{}/dockerloader/storage/v1/extracted", data_dir);
    let images_dir = format!("{}/dockerloader/storage/v1/images", data_dir);
    let blobs_dir = format!("{}/dockerloader/storage/v1/blobs", data_dir);
    assert_eq!(count_files_in_dir(&extracted_dir)?, 1);
    assert_eq!(count_files_in_dir(&images_dir)?, 1);

    // Create tmp files to verify tmp cleanup works
    println!("Creating tmp files to test cleanup");
    std::fs::write(format!("{}/tmp-testfile", images_dir), "test")?;
    std::fs::create_dir(format!("{}/tmp-testdir", blobs_dir))?;

    // Run again - cleanup should remove tmp files
    println!("Running to trigger cleanup of tmp files");
    let output = docker.run_dockerloader(&data_dir)?;
    assert!(output.contains("Version: 2.0"));

    // Verify tmp files are gone
    println!("Verifying tmp files were cleaned up");
    assert!(!std::fs::exists(format!("{}/tmp-testfile", images_dir))?);
    assert!(!std::fs::exists(format!("{}/tmp-testdir", blobs_dir))?);

    println!("Building version 3 with FAIL_INIT=1");
    docker.build_and_push(vec!["VERSION=3.0".into(), "FAIL_INIT=1".into()])?;

    println!("Fourth run - should try to update to v3 and fail");
    let result = docker.run_dockerloader(&data_dir);

    assert!(result.is_err(), "expected docker run to fail");
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("intentional failure for testing"));

    println!("Verifying symlink still points to v2");
    assert_symlink_contains(&data_dir, &extract_sha(&v2_target)?)?;

    println!("Fifth run - should skip v3 (failed update tracked) and run v2");
    let output = docker.run_dockerloader(&data_dir)?;
    assert!(output.contains("Version: 2.0"));
    assert!(output.contains("skipping update"));
    assert!(!output.contains("trial mode: true"));

    assert_symlink_contains(&data_dir, &extract_sha(&v2_target)?)?;

    println!("Test complete!");
    Ok(())
}

#[test]
#[serial_test::serial]
fn test_dockerloader_timeout() -> Result<()> {
    let workspace_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_str()
        .unwrap();
    let data_dir = format!("{}/dockerloader/data", workspace_dir);

    let _ = std::fs::remove_dir_all(&data_dir);

    let docker = Docker::new(workspace_dir);

    println!("Building dockerloader:testing");
    docker.build_dockerloader()?;

    println!("Building and pushing version 1");
    docker.build_and_push(vec!["VERSION=1.0".into()])?;

    println!("First run - download and run version 1");
    let output = docker.run_dockerloader(&data_dir)?;
    assert!(output.contains("Version: 1.0"));

    let v1_target = read_symlink_target(&data_dir)?;
    println!("V1 symlink target: {}", v1_target);

    println!("Building version 2 with TIMEOUT_INIT=1");
    docker.build_and_push(vec!["VERSION=2.0".into(), "TIMEOUT_INIT=1".into()])?;

    println!("Second run - should try v2 and timeout");
    let result = docker.run_dockerloader(&data_dir);

    assert!(result.is_err(), "expected docker run to fail");
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("DOCKERLOADER_TRIAL timeout hit"));

    println!("Verifying symlink still points to v1");
    assert_symlink_contains(&data_dir, &extract_sha(&v1_target)?)?;

    println!("Third run - should skip v2 and run v1");
    let output = docker.run_dockerloader(&data_dir)?;
    assert!(output.contains("Version: 1.0"));
    assert!(output.contains("skipping update"));

    println!("Timeout test complete!");
    Ok(())
}

#[test]
#[serial_test::serial]
fn test_background_update_loop() -> Result<()> {
    let workspace_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_str()
        .unwrap();
    let data_dir = format!("{}/dockerloader/data", workspace_dir);

    let _ = std::fs::remove_dir_all(&data_dir);

    let docker = Docker::new(workspace_dir);

    println!("Building dockerloader:testing");
    docker.build_dockerloader()?;

    println!("Building and pushing version 1 with LONG_RUNNING=1");
    docker.build_and_push(vec!["VERSION=1.0".into(), "LONG_RUNNING=1".into()])?;

    println!("Starting dockerloader in background with 3-second update interval");
    let container_id = docker.run_detached(&data_dir, "dockerloader-test", 3)?;
    println!("Container ID: {}", container_id);

    // Wait for container to start and mark ready
    println!("Waiting for version 1 to start...");
    std::thread::sleep(std::time::Duration::from_secs(5));

    // Check logs to verify v1 is running
    let logs = docker.logs(&container_id)?;
    assert!(logs.contains("Version: 1.0"));
    assert!(logs.contains("starting update loop"));

    // Verify symlink points to v1
    let v1_target = read_symlink_target(&data_dir)?;
    let v1_sha = extract_sha(&v1_target)?;
    println!("V1 running with SHA: {}", v1_sha);

    println!("Building and pushing version 2 with LONG_RUNNING=1");
    docker.build_and_push(vec!["VERSION=2.0".into(), "LONG_RUNNING=1".into()])?;

    println!("Waiting for background update check to detect v2 (up to 15 seconds)...");
    // The update interval is 3 seconds, so it should check within ~3-6 seconds
    // We'll wait up to 15 seconds to be safe
    let mut detected_update = false;
    for i in 0..15 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let logs = docker.logs(&container_id)?;
        if logs.contains("update available") {
            println!("Update detected after ~{} seconds", i + 1);
            detected_update = true;
            break;
        }
    }

    if !detected_update {
        let logs = docker.logs(&container_id)?;
        docker.stop(&container_id)?;
        anyhow::bail!("background update was not detected within 15 seconds. Logs:\n{}", logs);
    }

    println!("Waiting for v2 to start in trial mode...");
    std::thread::sleep(std::time::Duration::from_secs(3));

    // Check logs for v2
    let logs = docker.logs(&container_id)?;
    assert!(logs.contains("Version: 2.0"));
    assert!(logs.contains("trial mode: true"));
    assert!(logs.contains("trial successful"));

    // Verify symlink now points to v2
    let v2_target = read_symlink_target(&data_dir)?;
    let v2_sha = extract_sha(&v2_target)?;
    println!("V2 now running with SHA: {}", v2_sha);
    assert_ne!(v1_sha, v2_sha, "SHA should have changed from v1 to v2");

    // Test CLI passthrough
    println!("Testing CLI passthrough with docker exec");
    let cli_output = docker.exec(&container_id, vec!["cli".into()])?;
    assert!(cli_output.contains("Version: 2.0"));

    // Cleanup
    println!("Stopping container");
    docker.stop(&container_id)?;

    println!("Background update loop test complete!");
    Ok(())
}
