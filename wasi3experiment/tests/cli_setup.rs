use expectrl::session::Session;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

// Re-export the config structs for testing
// These match the structs in main.rs
#[derive(serde::Deserialize)]
struct Wasi3ExperimentConfig {
    listener: Option<ListenerConfig>,
    tunnel: Option<tunnel::client::ClientState>,
}

#[derive(serde::Deserialize)]
struct ListenerConfig {
    local: Option<LocalConfig>,
}

#[derive(serde::Deserialize)]
struct LocalConfig {
    address: String,
}

#[test]
fn test_cli_setup_prompts_and_creates_config() {
    // Create a temp directory for the config
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let config_path = temp_dir.path().join("config.json");

    // Get the path to the compiled binary
    let bin_path = get_binary_path();

    // Spawn the process with a PTY, setting CONFIG_PATH env var
    let mut cmd = Command::new(bin_path);
    cmd.arg("cli");
    cmd.arg("setup");
    cmd.env("CONFIG_PATH", config_path.to_string_lossy().as_ref());

    let mut session = Session::spawn(cmd).expect("failed to spawn process");

    // Set a timeout for expectations
    session.set_expect_timeout(Some(Duration::from_secs(5)));

    // Wait for and check the first prompt
    let result = session.expect("do you want to use tunnel mode?");
    assert!(
        result.is_ok(),
        "failed to find tunnel mode prompt: {:?}",
        result.err()
    );

    // Answer "no" to tunnel mode
    session.send_line("n").expect("failed to send answer");

    // Wait for and check the second prompt
    let result = session.expect("what address should the local listener use?");
    assert!(
        result.is_ok(),
        "failed to find address prompt: {:?}",
        result.err()
    );

    // Accept the default by pressing enter
    session.send_line("").expect("failed to send enter");

    // Wait for completion message
    let result = session.expect("setup complete!");
    assert!(
        result.is_ok(),
        "failed to find completion message: {:?}",
        result.err()
    );

    // Give it a moment to finish writing the file
    std::thread::sleep(Duration::from_millis(500));

    // Verify config file was created
    assert!(
        config_path.exists(),
        "config file was not created at {:?}",
        config_path
    );

    // Verify the config content using the actual structs
    let config_content = std::fs::read_to_string(&config_path).expect("failed to read config file");

    let config: Wasi3ExperimentConfig =
        serde_json::from_str(&config_content).expect("failed to parse config");

    // Check structure
    assert!(config.listener.is_some(), "listener should be present");
    let listener = config.listener.unwrap();
    assert!(listener.local.is_some(), "listener.local should be present");
    assert_eq!(
        listener.local.unwrap().address,
        "127.0.0.1:8080",
        "address should be the default"
    );
    assert!(
        config.tunnel.is_none(),
        "tunnel should be None for local-only mode"
    );
}

fn get_binary_path() -> PathBuf {
    // Get the path to the compiled test binary's directory
    let mut path = std::env::current_exe().expect("failed to get current exe");
    path.pop(); // Remove the test binary name

    // Check if we're in deps/ directory (we are during test execution)
    if path.ends_with("deps") {
        path.pop();
    }

    path.push("wasi3experiment");
    path
}
