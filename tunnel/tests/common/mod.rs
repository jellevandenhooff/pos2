use std::path::PathBuf;
use std::time::Duration;

pub fn get_binary_path() -> PathBuf {
    let mut path = std::env::current_exe().expect("failed to get current exe");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("tunnel");
    path
}

pub fn init_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub struct TestServerSetup {
    pub server_dir: PathBuf,
    pub http_port: u16,
    pub https_port: u16,
    pub quinn_port: u16,
    pub dns_port: u16,
    pub base_url: String,
    pub hostname: String,
}

impl TestServerSetup {
    pub fn new(prefix: &str) -> Self {
        let http_port = 8080;
        let https_port = 443;
        let quinn_port = 8444;
        let dns_port = 9999;

        let base_url = format!("http://127.0.0.1:{}", http_port);
        let hostname = format!("{}.test.local", prefix);

        // use unittest directory in repo (already in .gitignore)
        let server_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("unittest")
            .join(prefix);
        let _ = std::fs::remove_dir_all(&server_dir);
        std::fs::create_dir_all(&server_dir).expect("create unittest dir");

        Self {
            server_dir,
            http_port,
            https_port,
            quinn_port,
            dns_port,
            base_url,
            hostname,
        }
    }

    pub fn create_server_config(&self) -> tunnel::server::ServerConfig {
        tunnel::server::ServerConfig {
            http_listen_addr: Some(format!("0.0.0.0:{}", self.http_port)),
            web_base_url: self.base_url.clone(),
            https_listen_addr: format!("0.0.0.0:{}", self.https_port),
            quinn_listen_addr: format!("0.0.0.0:{}", self.quinn_port),
            dns_listen_addr: format!("0.0.0.0:{}", self.dns_port),
            dns_catalog_name: "test.local.".into(),
            quinn_endpoint: format!("{}:{}", self.hostname, self.quinn_port),
            hostname: self.hostname.clone(),
            public_ip: "172.19.0.10".into(),
            available_suffixes: vec![".test.local".into()],
            github_oauth2_client_id: "test_client_id".into(),
            github_oauth2_client_secret: "test_client_secret".into(),
            enable_acme: true,
            enable_test_login: true,
        }
    }

    pub fn write_config(&self) {
        let server_config = self.create_server_config();
        let config_path = self.server_dir.join("config.json");
        std::fs::write(
            &config_path,
            serde_json::to_string_pretty(&server_config).expect("serialize config"),
        )
        .expect("write config");
        println!("wrote config to: {}", config_path.display());
    }

    pub async fn spawn_server(&self) -> tokio::process::Child {
        println!("starting tunnel server in docker on {}", self.hostname);

        let container_name = "tunnel-test-server";

        let mut server_process = tokio::process::Command::new("docker")
            .arg("run")
            .arg("--network").arg("tunnel-test")
            .arg("--ip").arg("172.19.0.10")
            .arg("--dns").arg("172.19.0.2")
            .arg("--name").arg(container_name)
            .arg("-p").arg(format!("{}:{}", self.http_port, self.http_port))
            .arg("-p").arg(format!("{}:{}", self.https_port, self.https_port))
            .arg("-p").arg(format!("{}:{}/udp", self.quinn_port, self.quinn_port))
            .arg("-p").arg(format!("{}:{}/udp", self.dns_port, self.dns_port))
            .arg("-v").arg(format!("{}:/data", self.server_dir.display()))
            .arg("tunnel-test")
            .arg("/tunnel")
            .arg("--test")
            .arg("server")
            .arg("/data")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("failed to start server");

        tokio::time::sleep(Duration::from_millis(2000)).await;

        match server_process.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut out) = server_process.stdout.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = out.read_to_string(&mut stdout).await;
                }
                if let Some(mut err) = server_process.stderr.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = err.read_to_string(&mut stderr).await;
                }
                panic!(
                    "server exited early with status: {}\nstdout: {}\nstderr: {}",
                    status, stdout, stderr
                );
            }
            Ok(None) => println!("server is running"),
            Err(e) => panic!("error checking server status: {}", e),
        }

        server_process
    }
}
