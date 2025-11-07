mod common;

use expectrl::{session::Session, Regex as ExpectRegex};
use reqwest::Client;
use serde_json::json;
use std::process::Command;
use std::time::Duration;

fn log_session_output(prefix: &str, data: &[u8]) {
    let output = String::from_utf8_lossy(data);
    for line in output.lines() {
        println!("[{}] {}", prefix, line);
    }
}

#[tokio::test]
async fn test_tunnel_end_to_end() {
    common::init_crypto();

    // rebuild docker image with latest code
    println!("rebuilding tunnel-test docker image");
    let build_output = tokio::process::Command::new("docker")
        .args(&["build", "-t", "tunnel-test", "-f", "Dockerfile.tunnel-test", "."])
        .current_dir(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
        .output()
        .await
        .expect("failed to build docker image");

    if !build_output.status.success() {
        panic!(
            "docker build failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&build_output.stdout),
            String::from_utf8_lossy(&build_output.stderr)
        );
    }

    // clean up any lingering containers from previous runs
    let _ = tokio::process::Command::new("docker")
        .args(&["rm", "-f", "tunnel-test-server", "tunnel-test-client"])
        .output()
        .await;

    let setup = common::TestServerSetup::new("tunnel-e2e");

    let client_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("unittest")
        .join("client");
    let _ = std::fs::remove_dir_all(&client_dir);
    std::fs::create_dir_all(&client_dir).expect("create client dir");

    setup.write_config();

    let client_config = serde_json::json!({
        "endpoints": [format!("https://{}:{}", setup.hostname, setup.https_port)]
    });

    std::fs::write(
        client_dir.join("config.json"),
        serde_json::to_string_pretty(&client_config).expect("serialize config"),
    )
    .expect("write config");

    println!("step 1: starting tunnel server in test mode");
    let server_process = setup.spawn_server().await;

    let client = Client::new();

    println!("waiting for ACME certificate issuance");
    tokio::time::sleep(Duration::from_secs(5)).await;

    println!("step 2: start client setup with expectrl in docker");
    let container_name = "tunnel-test-client";

    let mut cmd = Command::new("docker");
    cmd.arg("run")
        .arg("-it")
        .arg("--network").arg("tunnel-test")
        .arg("--dns").arg("172.19.0.2")
        .arg("--name").arg(container_name)
        .arg("-v").arg(format!("{}:/data", client_dir.display()))
        .arg("tunnel-test")
        .arg("/tunnel")
        .arg("--test")
        .arg("client")
        .arg("/data");

    let mut session = Session::spawn(cmd).expect("failed to spawn client");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    println!("step 3: select tunnel server");
    let result = session
        .expect("what tunnel server")
        .await
        .expect("should prompt for tunnel server");
    log_session_output("tunnel-select", &result.as_bytes());
    session.send_line("").await.expect("select first option");

    println!("step 4: expect device code prompt");
    let result = session
        .expect("enter the code:")
        .await
        .expect("should show device code prompt");
    log_session_output("device-code", &result.as_bytes());

    let code_pattern = ExpectRegex(r"[A-Z0-9]{4}-[A-Z0-9]{4}");
    let result = session
        .expect(code_pattern)
        .await
        .expect("should find user code");
    log_session_output("user-code", &result.as_bytes());

    let user_code_str = String::from_utf8_lossy(&result.as_bytes());
    let user_code = regex::Regex::new(r"[A-Z0-9]{4}-[A-Z0-9]{4}")
        .unwrap()
        .find(&user_code_str)
        .expect("should extract user code")
        .as_str();

    println!("extracted user_code: {}", user_code);

    tokio::time::sleep(Duration::from_millis(100)).await;

    println!("step 5: test login to create session");
    let jar = reqwest::cookie::Jar::default();
    let client_with_cookies = Client::builder()
        .cookie_provider(std::sync::Arc::new(jar))
        .build()
        .expect("failed to build client");

    let login_response = client_with_cookies
        .post(format!("{}/login/test", setup.base_url))
        .json(&json!({"email": "test@example.com"}))
        .send()
        .await
        .expect("test login failed");

    assert_eq!(login_response.status(), 200);

    println!("step 6: get device token by user code");
    let device_tokens_response: serde_json::Value = client
        .post(format!("{}/login/device/code", setup.base_url))
        .send()
        .await
        .expect("device code request failed")
        .json()
        .await
        .expect("failed to parse response");

    let device_code = device_tokens_response["device_code"]
        .as_str()
        .expect("no device_code");

    println!("step 7: approve device code");
    let approve_response = client_with_cookies
        .post(format!("{}/login/device/authorize", setup.base_url))
        .form(&[("device_code", device_code), ("user_code", &user_code)])
        .send()
        .await
        .expect("approve request failed");

    assert!(approve_response.status().is_success());

    println!("step 8: select domain");
    let result = session
        .expect("what domain")
        .await
        .expect("should prompt for domain");
    log_session_output("domain-select", &result.as_bytes());

    session
        .send_line("")
        .await
        .expect("select first domain option (new domain)");

    println!("step 9: wait for suffix name prompt");
    let result = session
        .expect("please pick a name before the suffix")
        .await
        .expect("should prompt for domain prefix");
    log_session_output("domain-prefix-prompt", &result.as_bytes());

    session
        .send_line("test-client")
        .await
        .expect("send domain prefix");

    println!("step 10: confirm domain");
    let result = session
        .expect("continue with domain")
        .await
        .expect("should prompt for confirmation");
    log_session_output("domain-confirm", &result.as_bytes());

    session.send_line("yes").await.expect("confirm domain");

    println!("step 11: wait for tunnel connection to establish");
    tokio::time::sleep(Duration::from_secs(3)).await;

    println!("step 12: verify state.json was created");
    let state_path = client_dir.join("state.json");
    assert!(state_path.exists(), "state.json should be created");

    let state_content = std::fs::read_to_string(&state_path).expect("read state");
    let state: tunnel::client::ClientState =
        serde_json::from_str(&state_content).expect("parse state");

    let client_domain = &state.domain;
    println!("client domain: {}", client_domain);

    println!("step 13: make HTTPS request through tunnel using docker curl");
    let tunnel_url = format!("https://{}", client_domain);

    let curl_output = tokio::process::Command::new("docker")
        .arg("run")
        .arg("--rm")
        .arg("--network").arg("tunnel-test")
        .arg("--dns").arg("172.19.0.2")
        .arg("tunnel-test")
        .arg("sh")
        .arg("-c")
        .arg(format!("curl -f -s {}", tunnel_url))
        .output()
        .await
        .expect("failed to run curl in docker");

    assert!(
        curl_output.status.success(),
        "curl request should succeed, stderr: {}",
        String::from_utf8_lossy(&curl_output.stderr)
    );

    let body = String::from_utf8_lossy(&curl_output.stdout);
    println!("response from tunnel: {}", body);

    assert!(
        body.contains("Hello"),
        "response should contain greeting from client handler"
    );

    println!("success! full end-to-end tunnel test passed");

    drop(session);
    drop(server_process);
}
