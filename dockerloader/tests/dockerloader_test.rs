use anyhow::{Context, Result};
use std::process::Command;

fn docker(args: Vec<String>, cwd: &str) -> Result<String> {
    let mut cmd = Command::new("docker");
    cmd.args(args);

    cmd.current_dir(cwd);

    let output = cmd.output().context("failed to run docker command")?;

    if !output.status.success() {
        anyhow::bail!(
            "docker command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn dockerbuild_dockerloader(cwd: &str) -> Result<()> {
    docker(
        vec![
            "build".into(),
            "-t".into(),
            "dockerloader:testing".into(),
            "-f".into(),
            "Dockerfile.dockerloader".into(),
            ".".into(),
        ],
        cwd,
    )?;
    Ok(())
}

fn dockerbuild_and_push(build_args: Vec<String>, cwd: &str) -> Result<()> {
    let mut args = vec![
        "build".into(),
        "-t".into(),
        "localhost:5050/dockerloaded:testing".into(),
        "-f".into(),
        "Dockerfile.dockerloaded".into(),
    ];

    for build_arg in build_args {
        args.push("--build-arg".into());
        args.push(build_arg);
    }

    args.push(".".into());
    docker(args, cwd)?;
    docker(
        vec!["push".into(), "localhost:5050/dockerloaded:testing".into()],
        cwd,
    )?;
    Ok(())
}

fn dockerloader(data_dir: &str, cwd: &str) -> Result<String> {
    docker(
        vec![
            "run".into(),
            "--rm".into(),
            "-v".into(),
            format!("{}:/data", data_dir),
            "dockerloader:testing".into(),
        ],
        cwd,
    )
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
        println!("✓ Symlink points to {}", expected_sha);
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
    let data_dir = format!("{}/data/dockerloader", workspace_dir);

    let _ = std::fs::remove_dir_all(&data_dir);

    println!("Building dockerloader:testing");
    dockerbuild_dockerloader(workspace_dir)?;

    println!("Building and pushing version 1");
    dockerbuild_and_push(vec!["VERSION=1.0".into()], workspace_dir)?;

    println!("First run - should download version 1");
    let output = dockerloader(&data_dir, workspace_dir)?;
    assert!(output.contains("Version: 1.0"));

    println!("Building and pushing version 2");
    dockerbuild_and_push(vec!["VERSION=2.0".into()], workspace_dir)?;

    println!("Second run - should detect update and download version 2");
    let output = dockerloader(&data_dir, workspace_dir)?;
    assert!(output.contains("Version: 2.0"));
    assert!(output.contains("trial mode: true"));
    assert!(output.contains("update available"));
    assert!(output.contains("trial successful"));

    println!("Third run - should run version 2 without updating");
    let output = dockerloader(&data_dir, workspace_dir)?;
    assert!(output.contains("Version: 2.0"), "expected version 2.0");
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
    let output = dockerloader(&data_dir, workspace_dir)?;
    assert!(output.contains("Version: 2.0"));

    // Verify tmp files are gone
    println!("Verifying tmp files were cleaned up");
    assert!(!std::fs::exists(format!("{}/tmp-testfile", images_dir))?);
    assert!(!std::fs::exists(format!("{}/tmp-testdir", blobs_dir))?);

    println!("Building version 3 with FAIL_INIT=1");
    dockerbuild_and_push(
        vec!["VERSION=3.0".into(), "FAIL_INIT=1".into()],
        workspace_dir,
    )?;

    println!("Fourth run - should try to update to v3 and fail");
    let result = dockerloader(&data_dir, workspace_dir);

    assert!(result.is_err(), "expected docker run to fail");
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("intentional failure for testing"));

    println!("Verifying symlink still points to v2");
    assert_symlink_contains(&data_dir, &extract_sha(&v2_target)?)?;

    println!("Fifth run - should skip v3 (failed update tracked) and run v2");
    let output = dockerloader(&data_dir, workspace_dir)?;
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
    let data_dir = format!("{}/data/dockerloader", workspace_dir);

    let _ = std::fs::remove_dir_all(&data_dir);

    println!("Building dockerloader:testing");
    dockerbuild_dockerloader(workspace_dir)?;

    println!("Building and pushing version 1");
    dockerbuild_and_push(vec!["VERSION=1.0".into()], workspace_dir)?;

    println!("First run - download and run version 1");
    let output = dockerloader(&data_dir, workspace_dir)?;
    assert!(output.contains("Version: 1.0"));

    let v1_target = read_symlink_target(&data_dir)?;
    println!("V1 symlink target: {}", v1_target);

    println!("Building version 2 with TIMEOUT_INIT=1");
    dockerbuild_and_push(
        vec!["VERSION=2.0".into(), "TIMEOUT_INIT=1".into()],
        workspace_dir,
    )?;

    println!("Second run - should try v2 and timeout");
    let result = dockerloader(&data_dir, workspace_dir);

    assert!(result.is_err(), "expected docker run to fail");
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("DOCKERLOADER_TRIAL timeout hit"));

    println!("Verifying symlink still points to v1");
    assert_symlink_contains(&data_dir, &extract_sha(&v1_target)?)?;

    println!("Third run - should skip v2 and run v1");
    let output = dockerloader(&data_dir, workspace_dir)?;
    assert!(output.contains("Version: 1.0"));
    assert!(output.contains("skipping update"));

    println!("Timeout test complete!");
    Ok(())
}

// TODO: continuously running container test??
