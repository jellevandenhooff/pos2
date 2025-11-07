use reqwest::Client;
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn test_device_flow_with_test_login() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");

    let config = tunnel::server::ServerConfig {
        http_listen_addr: Some("127.0.0.1:0".into()),
        web_base_url: "http://127.0.0.1:8080".into(),
        https_listen_addr: "127.0.0.1:8443".into(),
        quinn_listen_addr: "127.0.0.1:4434".into(),
        dns_listen_addr: "127.0.0.1:5353".into(),
        dns_catalog_name: "test.local.".into(),
        quinn_endpoint: "test.local:4434".into(),
        hostname: "test.local".into(),
        public_ip: "127.0.0.1".into(),
        available_suffixes: vec![".test.local".into()],
        github_oauth2_client_id: "test_client_id".into(),
        github_oauth2_client_secret: "test_client_secret".into(),
        test_mode: true,
    };

    let env = tunnel::common::Environment::prod(temp_dir.path().to_string_lossy().into())
        .await
        .expect("failed to create environment");

    let db = tunnel::db::DB::new(&env).await.expect("failed to create db");

    let router = tunnel::web::make_router(
        env.clone(),
        db.clone(),
        config.available_suffixes.clone(),
        config.web_base_url.clone(),
        config.github_oauth2_client_id.clone(),
        config.github_oauth2_client_secret.clone(),
        config.test_mode,
    )
    .await
    .expect("failed to create router");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind");
    let addr = listener.local_addr().expect("failed to get address");

    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("server failed");
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = Client::new();
    let base_url = format!("http://{}", addr);

    println!("step 1: request device code");
    let device_response: serde_json::Value = client
        .post(format!("{}/login/device/code", base_url))
        .send()
        .await
        .expect("device code request failed")
        .json()
        .await
        .expect("failed to parse device response");

    let device_code = device_response["device_code"]
        .as_str()
        .expect("no device_code");
    let user_code = device_response["user_code"]
        .as_str()
        .expect("no user_code");

    println!("got device_code={} user_code={}", device_code, user_code);

    println!("step 2: test login to create session");
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

    println!("step 3: approve device code");
    let approve_response = client_with_cookies
        .post(format!("{}/login/device/authorize", base_url))
        .form(&[("device_code", device_code), ("user_code", user_code)])
        .send()
        .await
        .expect("approve request failed");

    assert!(
        approve_response.status().is_success(),
        "approval should succeed"
    );

    println!("step 4: poll for access token");
    let token_response: serde_json::Value = client
        .post(format!("{}/login/oauth/access_token", base_url))
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", "test_client"),
        ])
        .send()
        .await
        .expect("token request failed")
        .json()
        .await
        .expect("failed to parse token response");

    assert!(
        token_response.get("access_token").is_some(),
        "should receive access token: {:?}",
        token_response
    );

    println!("success! got access token");
}
