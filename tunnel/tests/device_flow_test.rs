use expectrl::session::Session;
use reqwest::Client;
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

fn get_binary_path() -> PathBuf {
    let mut path = std::env::current_exe().expect("failed to get current exe");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("tunnel");
    path
}

fn log_session_output(prefix: &str, data: &[u8]) {
    let output = String::from_utf8_lossy(data);
    for line in output.lines() {
        println!("[{}] {}", prefix, line);
    }
}

#[tokio::test]
async fn test_device_flow_end_to_end() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let server_dir = tempfile::tempdir().expect("failed to create server temp dir");
    let client_dir = tempfile::tempdir().expect("failed to create client temp dir");

    // use random ports to avoid conflicts
    let http_port = portpicker::pick_unused_port().expect("no free port");
    let https_port = portpicker::pick_unused_port().expect("no free port");
    let quinn_port = portpicker::pick_unused_port().expect("no free port");
    let dns_port = portpicker::pick_unused_port().expect("no free port");

    let base_url = format!("http://127.0.0.1:{}", http_port);

    let server_config = tunnel::server::ServerConfig {
        http_listen_addr: Some(format!("127.0.0.1:{}", http_port)),
        web_base_url: base_url.clone(),
        https_listen_addr: format!("127.0.0.1:{}", https_port),
        quinn_listen_addr: format!("127.0.0.1:{}", quinn_port),
        dns_listen_addr: format!("127.0.0.1:{}", dns_port),
        dns_catalog_name: "test.local.".into(),
        quinn_endpoint: format!("test.local:{}", quinn_port),
        hostname: "test.local".into(),
        public_ip: "127.0.0.1".into(),
        available_suffixes: vec![".test.local".into()],
        github_oauth2_client_id: "test_client_id".into(),
        github_oauth2_client_secret: "test_client_secret".into(),
        test_mode: true,
    };

    std::fs::write(
        server_dir.path().join("config.json"),
        serde_json::to_string_pretty(&server_config).expect("serialize config"),
    )
    .expect("write config");

    let client_config = serde_json::json!({
        "endpoints": [base_url.clone()]
    });

    std::fs::write(
        client_dir.path().join("config.json"),
        serde_json::to_string_pretty(&client_config).expect("serialize config"),
    )
    .expect("write config");

    let bin_path = get_binary_path();

    println!("starting tunnel server in test mode");
    let mut server_process = tokio::process::Command::new(&bin_path)
        .arg("server")
        .arg(server_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to start server");

    // give server time to start
    tokio::time::sleep(Duration::from_millis(2000)).await;

    // check if server is running
    match server_process.try_wait() {
        Ok(Some(status)) => panic!("server exited early with status: {}", status),
        Ok(None) => println!("server is running"),
        Err(e) => panic!("error checking server status: {}", e),
    }

    let client = Client::new();

    println!("step 1: start client setup with expectrl");
    let mut cmd = Command::new(&bin_path);
    cmd.arg("client");
    cmd.arg(client_dir.path());

    let mut session = Session::spawn(cmd).expect("failed to spawn client");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    println!("step 2: select tunnel server");
    let result = session
        .expect("what tunnel server")
        .expect("should prompt for tunnel server");
    log_session_output("tunnel-select", &result.as_bytes());
    session.send_line("").expect("select first option");

    println!("step 3: expect device code prompt");
    let result = session
        .expect("enter the code:")
        .expect("should show device code prompt");
    log_session_output("device-code", &result.as_bytes());

    // wait a bit for the code to be printed
    tokio::time::sleep(Duration::from_millis(500)).await;

    // read any remaining output
    let mut buffer = vec![0u8; 1024];
    let mut additional_output = Vec::new();
    if let Ok(available) = session.try_read(&mut buffer) {
        if available > 0 {
            additional_output = buffer[..available].to_vec();
            log_session_output("device-code-cont", &additional_output);
        }
    }

    // extract the user code using regex (format: XXXX-XXXX)
    let full_output = result.as_bytes();
    let combined: Vec<u8> = [full_output, &additional_output].concat();
    let combined_str = String::from_utf8_lossy(&combined);
    let user_code = regex::Regex::new(r"[A-Z0-9]{4}-[A-Z0-9]{4}")
        .unwrap()
        .find(&combined_str)
        .expect("should find user code in output")
        .as_str();

    println!("extracted user_code: {}", user_code);

    tokio::time::sleep(Duration::from_millis(100)).await;

    println!("step 3: test login to create session");
    let jar = reqwest::cookie::Jar::default();
    let client_with_cookies = Client::builder()
        .cookie_provider(std::sync::Arc::new(jar))
        .build()
        .expect("failed to build client");

    let login_response = client_with_cookies
        .post(format!("{}/login/test", base_url))
        .json(&json!({"email": "test@example.com"}))
        .send()
        .await
        .expect("test login failed");

    assert_eq!(
        login_response.status(),
        200,
        "test login should succeed"
    );

    println!("step 4: get device token by user code");
    let device_tokens_response: serde_json::Value = client
        .post(format!("{}/login/device/code", base_url))
        .send()
        .await
        .expect("device code request failed")
        .json()
        .await
        .expect("failed to parse response");

    let device_code = device_tokens_response["device_code"]
        .as_str()
        .expect("no device_code");

    println!("step 5: approve device code");
    let approve_response = client_with_cookies
        .post(format!("{}/login/device/authorize", base_url))
        .form(&[("device_code", device_code), ("user_code", &user_code)])
        .send()
        .await
        .expect("approve request failed");

    assert!(
        approve_response.status().is_success(),
        "approval should succeed, got: {}",
        approve_response.status()
    );

    println!("step 6: select domain");
    let result = session
        .expect("what domain")
        .expect("should prompt for domain");
    log_session_output("domain-select", &result.as_bytes());

    session.send_line("").expect("select first domain option (new domain)");

    // after selecting "new domain", it prompts for the prefix before the suffix
    println!("step 6b: wait for suffix name prompt");
    let result = session
        .expect("please pick a name before the suffix")
        .expect("should prompt for domain prefix");
    log_session_output("domain-prefix-prompt", &result.as_bytes());

    session.send_line("test-client").expect("send domain prefix");

    // it will show the full domain and ask for confirmation
    println!("step 6c: confirm domain");
    let result = session
        .expect("continue with domain")
        .expect("should prompt for confirmation");
    log_session_output("domain-confirm", &result.as_bytes());

    session.send_line("yes").expect("confirm domain");

    println!("step 7: wait a moment for state.json to be written");
    tokio::time::sleep(Duration::from_millis(2000)).await;

    println!("step 8: verify state.json was created");
    let state_path = client_dir.path().join("state.json");

    // debug: check what files exist
    if !state_path.exists() {
        println!("state.json not found, listing client dir:");
        for entry in std::fs::read_dir(client_dir.path()).expect("read dir") {
            println!("  {:?}", entry.expect("entry").file_name());
        }
    }

    assert!(state_path.exists(), "state.json should be created");

    let state_content = std::fs::read_to_string(&state_path).expect("read state");
    let state: tunnel::client::ClientState =
        serde_json::from_str(&state_content).expect("parse state");

    assert_eq!(state.endpoint, base_url);
    assert!(!state.token.is_empty(), "should have auth token");
    assert!(!state.domain.is_empty(), "should have domain");

    println!("success! full end-to-end test passed");

    drop(session); // kill client process
    drop(server_process); // kill server process
}
