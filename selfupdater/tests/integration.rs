use anyhow::Result;
use tokio::process;

#[tokio::test]
#[test_log::test]
async fn help() -> Result<()> {
    let _docker = selfupdater::docker::Client::new().await?;

    let dir = concat!(env!("CARGO_MANIFEST_DIR"));

    let mut child = process::Command::new("docker")
        .arg("build")
        .arg(".")
        .kill_on_drop(true)
        .spawn()
        .expect("failed to spawn");

    // Await until the command completes
    let status = child.wait().await?;
    println!("the command exited with: {}", status);
    // process

    println!("dir: {}", dir);

    println!("hello");

    Ok(())
}
