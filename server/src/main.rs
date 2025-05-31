pub mod models;

use self::models::*;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use axum::extract::{Path, Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json};
use axum_extra::extract::cookie::Cookie;
use axum_extra::extract::{CookieJar, Host};
use chrono::Utc;
use component::testapp::custom_host::Value;
use oci_client::client::{ClientProtocol, ImageLayer};
use oci_wasm::WasmConfig;
use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::params_from_iter;
use rusqlite::types::ValueRef;
use sha2::Digest;
use tokio::task;
use tokio::time::sleep;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Result, Store};
use wasmtime_wasi::p2::{IoView, WasiCtx, WasiCtxBuilder, WasiView};

const LOGIN_URL: &str = "https://laptop.pos2.jelle.dev";

struct WasmHostState {
    ctx: WasiCtx,
    table: ResourceTable,
    name: String,
    app: UserAppModel,
}

impl IoView for WasmHostState {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}
impl WasiView for WasmHostState {
    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.ctx
    }
}

wasmtime::component::bindgen!({
    path: "../wit",
    with: {
        "wasi": wasmtime_wasi::p2::bindings,
    },
    async: true,
});

impl component::testapp::custom_host::Host for WasmHostState {
    async fn huh(&mut self) -> String {
        return format!("huh from host to {}", self.name).to_string();
    }
    async fn query(
        &mut self,
        query: String,
        args: Vec<component::testapp::custom_host::Value>,
    ) -> Option<component::testapp::custom_host::Rows> {
        let conn = rusqlite::Connection::open(format!(
            "./data/users/{}/apps/{}/db.sqlite3",
            self.app.user_id,
            name_to_path(&self.app.name).ok()?,
        ))
        .ok()?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS test (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL
            )",
            (),
        )
        .ok()?;

        let count: i64 = conn
            .query_row_and_then("SELECT COUNT(*) FROM test", [], |row| row.get(0).into())
            .ok()?;

        if count == 0 {
            conn.execute(
                "INSERT INTO test (id, name) VALUES (1, \"jim\"), (2, \"bob\")",
                (),
            )
            .ok()?;
        }

        let mapped_args = params_from_iter(args.iter().map(|x| match x {
            Value::Int(x) => rusqlite::types::Value::Integer(*x),
            Value::Text(x) => rusqlite::types::Value::Text(x.into()),
            _ => panic!("help"),
        }));

        let mut stmt = conn.prepare(&query).ok()?;
        let column_count = stmt.column_count(); // this can change???
        let mut iter = stmt.query(mapped_args).ok()?;
        let mut ret = vec![];
        loop {
            let next = iter.next().ok()?;
            if next.is_none() {
                break;
            }
            let inner = next.unwrap();
            let mut vals = vec![];
            for i in 0..column_count {
                let val = inner.get_ref(i).ok()?;
                let mapped = match val {
                    ValueRef::Integer(x) => Value::Int(x),
                    ValueRef::Text(s) => Value::Text(str::from_utf8(s).ok()?.to_string()),
                    _ => panic!("help"),
                };
                vals.push(mapped);
            }
            ret.push(vals);
        }
        return Some(ret);
    }
}

async fn run(app: &UserAppModel) -> Result<String> {
    let mut config = Config::default();
    config.async_support(true);

    let engine = Engine::new(&config)?;
    let component = Component::from_file(&engine, &app.path)?;

    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    component::testapp::custom_host::add_to_linker(&mut linker, |state: &mut WasmHostState| state)?;

    let mut builder = WasiCtxBuilder::new();
    builder.inherit_stdout();
    builder.inherit_stderr();

    let mut store = Store::new(
        &engine,
        WasmHostState {
            ctx: builder.build(),
            table: ResourceTable::new(),
            app: app.clone(),
            name: "jimbob".to_string(),
        },
    );
    let bindings = Example::instantiate_async(&mut store, &component, &linker).await?;
    bindings.call_hello_world(&mut store).await
}

async fn download_app(_app: &RegistryApp, app_version: &RegistryAppVersion) -> Result<String> {
    let mut oci_config = oci_client::client::ClientConfig::default();
    oci_config.protocol = ClientProtocol::HttpsExcept(vec!["localhost:5050".into()]);
    let oci_client = oci_client::client::Client::new(oci_config);
    let wasm_client = oci_wasm::WasmClient::new(oci_client);
    let reference: oci_spec::distribution::Reference = app_version.oci_reference.parse()?;
    let (oci_manifest, wasm_config, checksum) = wasm_client
        .pull_manifest_and_config(&reference, &oci_client::secrets::RegistryAuth::Anonymous)
        .await?;
    println!("wasm config: {:?}", wasm_config);
    println!("checksum: {:?}", checksum);
    let n = wasm_config.layer_digests.len();
    if n != 1 {
        anyhow::bail!("expected 1 layer, got {}", n)
    }
    let m = oci_manifest.layers.len();
    if m != n {
        anyhow::bail!("expected matching layers, got {} and {}", n, m)
    }

    let wasm_path = format!("./data/apps/{}.wasm", checksum);
    tokio::fs::create_dir_all(std::path::Path::new(&wasm_path).parent().unwrap()).await?;

    let mut file = tokio::fs::File::create(&wasm_path).await?;
    wasm_client
        .pull_blob(&reference, &oci_manifest.layers[0], &mut file)
        .await?;

    Ok(wasm_path)
}

async fn install_app(
    storage: &Storage,
    app: &RegistryApp,
    app_version: &RegistryAppVersion,
    user_id: &i32,
) -> Result<()> {
    let wasm_path = download_app(app, app_version).await?;

    let db_path = format!(
        "./data/users/{}/apps/{}/db.sqlite3",
        user_id,
        name_to_path(&app.name)?
    );
    tokio::fs::create_dir_all(std::path::Path::new(&db_path).parent().unwrap()).await?;

    storage
        .insert_user_app(&NewUserApp {
            user_id: user_id,
            name: &app.name,
            version: &app_version.version,
            path: &wasm_path,
        })
        .await?;

    Ok(())
}

async fn update_app(
    storage: &Storage,
    app: &RegistryApp,
    app_version: &RegistryAppVersion,
    user_id: &i32,
) -> Result<()> {
    let wasm_path = download_app(app, app_version).await?;

    /*
    let db_path = format!(
        "./data/users/{}/apps/{}/db.sqlite3",
        user_id,
        name_to_path(&app.name)?
    );
    tokio::fs::create_dir_all(std::path::Path::new(&db_path).parent().unwrap()).await?;
    */

    storage
        .update_user_app(&UpdateUserApp {
            user_id: user_id,
            name: &app.name,
            version: &app_version.version,
            path: &wasm_path,
        })
        .await?;

    Ok(())
}

#[derive(Clone)]
struct ServerState {
    storage: Storage,
    registry_client: RegistryClient,
}

#[derive(Clone)]
struct UserInfo {
    name: Option<String>,
    session_id: Option<String>,
    id: Option<i32>,
}

async fn auth_middleware(
    state: State<ServerState>,
    mut req: Request,
    next: axum::middleware::Next,
) -> Result<Response, axum::http::StatusCode> {
    /*
    let auth_header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|header| header.to_str().ok());

    let auth_header = if let Some(auth_header) = auth_header {
        auth_header
    } else {
        return Err(axum::http::StatusCode::UNAUTHORIZED);
    };
    */

    let mut username: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut id: Option<i32> = None;

    let mut cookies = CookieJar::from_headers(req.headers());

    // __Host- ?
    if let Some(session_id_cookie) = cookies.get("session_id") {
        if let Some((session, user)) = state
            .storage
            .get_user_for_session(session_id_cookie.value())
            .await
            .unwrap()
        {
            session_id = Some(session.id);
            username = Some(user.name);
            id = Some(user.id);
        } else {
            cookies = cookies.remove("session_id");
        }
    }

    req.extensions_mut().insert(UserInfo {
        name: username,
        session_id: session_id,
        id: id,
    });

    // if let Some(current_user) = authorize_current_user(auth_header).await {
    // insert the current user into a request extension so the handler can
    // extract it
    Ok((cookies, next.run(req).await).into_response())
    // } else {
    // Err(axum::http::StatusCode::UNAUTHORIZED)
    // }
}

async fn run_handler(
    Path(name): Path<String>,
    Extension(user): Extension<UserInfo>,
    State(state): State<ServerState>,
) -> axum::response::Html<String> {
    // let apps = state.storage.list_apps().await.unwrap();
    let apps = state
        .storage
        .list_user_apps_by_user_id(&user.id.unwrap())
        .await
        .unwrap();
    let user_app = apps
        .into_iter()
        .filter(|app| name_to_path(&app.name).unwrap() == name)
        .next()
        .unwrap();
    axum::response::Html(run(&user_app).await.unwrap())
}

async fn install_handler(
    Path((name, version)): Path<(String, String)>,
    Extension(user): Extension<UserInfo>,
    State(state): State<ServerState>,
) -> Response {
    let registry_apps = state.registry_client.fetch_apps().await.unwrap();
    let app = registry_apps
        .iter()
        .filter(|app| name_to_path(&app.name).unwrap() == name) // && app.version == version)
        .next()
        .unwrap();

    let app_version = app
        .versions
        .iter()
        .filter(|app_version| app_version.version == version)
        .next()
        .unwrap();

    install_app(&state.storage, app, app_version, &user.id.unwrap())
        .await
        .unwrap();

    axum::response::Redirect::temporary("/").into_response()
}

async fn update_handler(
    Path((name, version)): Path<(String, String)>,
    Extension(user): Extension<UserInfo>,
    State(state): State<ServerState>,
) -> Response {
    let registry_apps = state.registry_client.fetch_apps().await.unwrap();
    let app = registry_apps
        .iter()
        .filter(|app| name_to_path(&app.name).unwrap() == name) // && app.version == version)
        .next()
        .unwrap();

    let app_version = app
        .versions
        .iter()
        .filter(|app_version| app_version.version == version)
        .next()
        .unwrap();

    update_app(&state.storage, app, app_version, &user.id.unwrap())
        .await
        .unwrap();

    axum::response::Redirect::temporary("/").into_response()
}

async fn login_handler(
    mut cookies: CookieJar,
    Path(user_id): Path<i32>,
    State(state): State<ServerState>,
) -> Response {
    // TODO: logout if logged in?

    let user = state.storage.get_user_by_id(&user_id).await.unwrap();
    let session = state.storage.create_session_for_user(&user).await.unwrap();

    cookies = cookies.add(
        Cookie::build(("session_id", session.id))
            .path("/")
            // .secure(true) // safari doesn't like on localhost?
            .http_only(true)
            .max_age(cookie::time::Duration::hours(24))
            .same_site(cookie::SameSite::Lax)
            .build(),
    );

    (cookies, axum::response::Redirect::temporary("/")).into_response()
}

async fn logout_handler(
    mut cookies: CookieJar,
    Extension(user): Extension<UserInfo>,
    State(state): State<ServerState>,
) -> Response {
    if let Some(session_id) = &user.session_id {
        state.storage.delete_session(session_id).await.unwrap();
    }
    cookies = cookies.remove("session_id");

    (cookies, axum::response::Redirect::temporary(LOGIN_URL)).into_response()
}

async fn index_handler(
    State(state): State<ServerState>,
    Extension(user): Extension<UserInfo>,
    Host(host): Host,
) -> Response {
    // let apps = state.storage.list_apps().await.unwrap();
    // let mut app_names: Vec<String> = apps.keys().cloned().collect();
    // app_names.sort();

    // TODO: redirect based on auth status???

    let available_apps = state.registry_client.fetch_apps().await.unwrap();

    let users = state.storage.list_users().await.unwrap();

    let mut user_apps: Vec<UserAppModel> = vec![];
    if let Some(user_id) = user.id {
        user_apps = state
            .storage
            .list_user_apps_by_user_id(&user_id)
            .await
            .unwrap();
    }

    let available_apps_with_latest: Vec<_> = available_apps
        .iter()
        .map(|app| (app, app.versions.last().unwrap()))
        .collect();

    let installed_user_apps: HashMap<_, _> = user_apps.iter().map(|app| (&app.name, app)).collect();

    let tmpl = maud::html!(
        (maud::DOCTYPE)
        html {
            head {
                title {
                    "hello"
                }
            }
            body {
                h1 { "pos" }
                p { "you are visiting " (host) }
                @if let Some(name) = &user.name {
                    p { "hello user " (name) }
                    a href="/logout" { "Logout" }
                }
                div {
                    h2 { "Installed" }
                    /*
                    ul {
                        @for name in app_names {
                            li {
                                a href=(format!("/apps/{}", name)) { (name) }
                            }
                        }
                    }
                    */
                    ul {
                        @for app in &user_apps {
                            li {
                                a href=(format!("/apps/{}", name_to_path(&app.name).unwrap())) { (app.name) " " (app.version) }
                            }
                        }
                    }

                }
                @if user.name != None {
                    div {
                        h2 { "Available" }
                        ul {
                            @for (app, version) in available_apps_with_latest {
                                // @for version in app.versions {
                                    li {
                                        (app.name) " " (version.version)
                                        @if let Some(installed) = installed_user_apps.get(&app.name) {
                                            @if installed.version != version.version {
                                                a href=(format!("/update/{}/{}", name_to_path(&app.name).unwrap(), version.version)) { "update" }
                                            }
                                        } @else {
                                            a href=(format!("/install/{}/{}", name_to_path(&app.name).unwrap(), version.version)) { "install" }
                                        }
                                    }
                                // }
                            }
                        }
                    }
                }
                div {
                    h2 { "Login" }
                    @for user in users {
                        a href=(format!("https://{}/login/{}", user.name, user.id)) { (user.name) }
                    }
                }
            }
        }
    );
    axum::response::Html(tmpl.into_string()).into_response()
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct RegistryApp {
    name: String, // must be a url-like thing like a go package????
    // description [pull from OCI?]? other junk [author, repo? desc? screenshots?]? sha256?
    //
    // how do we store multiple version(s) for app(s)?? release channels??
    versions: Vec<RegistryAppVersion>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct RegistryAppVersion {
    oci_reference: String, // just use type directly?
    version: String,
}

struct Registry {
    storage: Storage,
}

impl Registry {
    fn new() -> Result<Self> {
        Ok(Registry {
            storage: Storage::new()?,
        })
    }
}

async fn registry_index_handler(Extension(state): Extension<Arc<Registry>>) -> Response {
    let app_models = state.storage.list_registry_apps().await.unwrap();

    let mut apps_map = HashMap::<String, RegistryApp>::new();

    for row in app_models.into_iter() {
        let app = apps_map
            .entry(row.name)
            .or_insert_with_key(|key| RegistryApp {
                name: key.clone(),
                versions: vec![],
            });
        app.versions.push(RegistryAppVersion {
            oci_reference: row.oci_reference,
            version: row.version,
        });
    }

    let mut apps: Vec<RegistryApp> = apps_map.into_values().collect();
    apps.sort_by(|a, b| a.name.cmp(&b.name));

    Json(apps).into_response()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PublishApp {
    name: String,
    version: String,
    oci_reference: String,
}

async fn registry_publish_handler(
    Extension(state): Extension<Arc<Registry>>,
    Json(payload): Json<PublishApp>,
) -> Response {
    if !check_name(&payload.name) {
        return (axum::http::StatusCode::BAD_REQUEST).into_response();
    }

    let id = state
        .storage
        .insert_registry_app(&NewRegistryApp {
            name: &payload.name,
            version: &payload.version,
            oci_reference: &payload.oci_reference,
        })
        .await
        .unwrap();

    // check manifest exists?
    // check name?
    // compare info w/ manifest?

    Json(id).into_response()
}

fn get_auth(
    reference: &oci_spec::distribution::Reference,
) -> anyhow::Result<oci_client::secrets::RegistryAuth> {
    /*
    match (self.username, self.password) {
        (Some(username), Some(password)) => {
            Ok(oci_client::secrets::RegistryAuth::Basic(username, password))
        }
        (None, None) => {
    */
    let server_url = get_docker_config_auth_key(reference);
    match docker_credential::get_credential(server_url) {
        Ok(docker_credential::DockerCredential::UsernamePassword(username, password)) => {
            return Ok(oci_client::secrets::RegistryAuth::Basic(username, password));
        }
        Ok(docker_credential::DockerCredential::IdentityToken(_)) => {
            return Err(anyhow::anyhow!("identity tokens not supported"));
        }
        Err(err) => {
            tracing::debug!("Failed to look up OCI credentials with key `{server_url}`: {err}");
        }
    }
    Ok(oci_client::secrets::RegistryAuth::Anonymous)
    /*
        }
        _ => Err(anyhow::anyhow!("Must provide both a username and password")),
    }
                */
}

/// Translate the registry into a key for the auth lookup.
fn get_docker_config_auth_key(reference: &oci_spec::distribution::Reference) -> &str {
    match reference.resolve_registry() {
        "index.docker.io" => "https://index.docker.io/v1/", // Default registry uses this key.
        other => other, // All other registries are keyed by their domain name without the `https://` prefix or any path suffix.
    }
}

impl Registry {}

#[derive(Clone)]
struct RegistryClient {
    url: String,
}

impl RegistryClient {
    fn new() -> Result<Self> {
        Ok(RegistryClient {
            url: "http://localhost:8080/registry".into(),
        })
    }

    async fn fetch_apps(&self) -> Result<Vec<RegistryApp>> {
        let apps = reqwest::get(&self.url)
            .await?
            .json::<Vec<RegistryApp>>()
            .await?;
        Ok(apps)
    }
}

fn check_name(name: &str) -> bool {
    static RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"^[a-zA-Z0-9-]+(\.[a-zA-Z0-9-]+)*(/[a-zA-Z0-9-]+)+$").unwrap());
    RE.is_match(name)
}

fn name_to_path(name: &str) -> Result<String> {
    if !check_name(name) {
        bail!("bad name");
    }
    Ok(name.replace("-", "--").replace("/", "-"))
}

fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", sha2::Sha256::digest(bytes))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct AppPublishConfig {
    name: String,
    wasm_filename: String,
    description: String,
    oci_repository: String,
    publish_registry: String,
}

async fn poller() {
    loop {
        // sleep(Duration::from_millis(5_000)).await;
        sleep(Duration::from_secs(60)).await;
        println!("poll");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let command = args.get(1).context("need an argument")?;
    if command == "serve" {
        let state = ServerState {
            storage: Storage::new()?,
            registry_client: RegistryClient::new()?,
        };

        let app = axum::Router::new()
            .route("/apps/{name}", get(run_handler))
            .route("/install/{name}/{version}", get(install_handler))
            .route("/update/{name}/{version}", get(update_handler))
            .route("/login/{user_id}", get(login_handler))
            .route("/logout", get(logout_handler))
            .route("/registry", get(registry_index_handler))
            .route("/registry/publish", post(registry_publish_handler))
            .route("/", get(index_handler))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                auth_middleware,
            ))
            .layer(Extension(Arc::new(Registry::new()?)))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
        task::spawn(poller());
        axum::serve(listener, app).await?;
        return Ok(());
    } else if command == "publish" {
        // read app.json to determine some stuff here?
        let config_path = args.get(2).context("need path to publis")?;
        // "../testjsapp/app.json";
        let dir = std::path::Path::new(config_path).parent().context("huh")?;
        let config = tokio::fs::read_to_string(config_path).await?;
        let parsed_config: AppPublishConfig = serde_json::from_str(&config)?;

        let version = format!("{}", chrono::Utc::now().format("%Y%m%dT%H%M"));
        let ref_string = format!("{}:{}", parsed_config.oci_repository, version);

        let reference: oci_spec::distribution::Reference = ref_string.parse()?;
        let auth = get_auth(&reference)?;

        let mut oci_config = oci_client::client::ClientConfig::default();
        oci_config.protocol = ClientProtocol::HttpsExcept(vec!["localhost:5050".into()]);
        let oci_client = oci_client::client::Client::new(oci_config);
        let wasm_client = oci_wasm::WasmClient::new(oci_client);

        let wasm_path = dir.join(parsed_config.wasm_filename);
        let data = tokio::fs::read(wasm_path).await?;
        let digest = vec![sha256_digest(&data)];

        // versioning (for now?): date time, git hash, check clean

        wasm_client
            .push(
                &reference,
                &auth,
                ImageLayer {
                    data: data,
                    media_type: oci_wasm::WASM_LAYER_MEDIA_TYPE.into(),
                    annotations: None,
                },
                // ohhh... there's a function to create a wasm config from the bytes. use?
                WasmConfig {
                    os: "wasip2".into(),
                    architecture: "wasm".into(),
                    author: None,
                    created: Utc::now(),
                    component: Some(oci_wasm::Component {
                        target: None,
                        exports: vec![],
                        imports: vec![],
                    }),
                    layer_digests: digest,
                },
                None,
            )
            .await?;

        let client = reqwest::Client::new();
        let id = client
            .post(parsed_config.publish_registry)
            .json(&PublishApp {
                name: parsed_config.name,
                version: version,
                oci_reference: ref_string,
            })
            .send()
            .await?
            .json::<i32>()
            .await?;
        println!("got id {}", id);
    } else {
        println!("unknown command {}", command);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_name() {
        assert!(!check_name("a"));
        assert!(!check_name("a/"));
        assert!(check_name("a/b"));
        assert!(check_name("a0/b"));
        assert!(check_name("a-/b"));
        assert!(!check_name("a_/b"));
        assert!(!check_name("a/b.c"));
        assert!(check_name("a.b/c"));
        assert!(check_name("a.b/c/d"));
        assert!(check_name("aA0-.b-/cD-0/d"));
    }

    #[test]
    fn test_name_to_path() {
        assert_eq!(name_to_path("a/b").unwrap(), "a-b");
        assert_eq!(name_to_path("a.b/c/d").unwrap(), "a.b-c-d");
        assert_eq!(name_to_path("a.b/c-d/e--f").unwrap(), "a.b-c--d-e----f");
    }
}
