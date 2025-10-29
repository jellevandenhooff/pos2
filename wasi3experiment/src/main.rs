use anyhow::bail;
use axum::Extension;
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use tokio_util::sync::CancellationToken;
use wasmtime::component::{Accessor, Linker, ResourceTable};
use wasmtime::{Config, Engine, Result, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p3::{DefaultWasiHttpCtx, Request, WasiHttpCtxView, WasiHttpView};

mod generated;
mod wasmtimewrapper;

mod sqlite;

struct ServerState {
    instances: Mutex<HashMap<String, Arc<WasmInstance>>>,
}

struct WasmInstance {
    sender: wasmtimewrapper::WorkSender<MyState, generated::App>,
}

impl WasmInstance {
    async fn initialize(&self) -> Result<()> {
        self.sender
            .submit(
                async move |store: &Accessor<MyState>, proxy: &generated::App| {
                    let (result, _task) = proxy.call_initialize(store).await?;
                    if let Err(_) = result {
                        bail!("failed to start worker");
                    }
                    Ok(())
                },
            )
            .await
            .await??;
        Ok(())
    }

    async fn handle(
        &self,
        req: http::Request<axum::body::Body>,
    ) -> Result<http::Response<http_body_util::combinators::UnsyncBoxBody<bytes::Bytes, ErrorCode>>>
    {
        let result_future = self
            .sender
            .submit(
                async move |store: &Accessor<MyState>, proxy: &generated::App| {
                    tracing::info!("run handler body");

                    let (req, body) = req.into_parts();
                    let body = body.map_err(|_| ErrorCode::HttpProtocolError);
                    let req = http::Request::from_parts(req, body);

                    let (request, request_io_result) = Request::from_http(req);

                    let (res, _task) = proxy.handle(store, request).await.unwrap().unwrap();

                    let res = store
                        .with(|mut store| res.into_http(&mut store, request_io_result))
                        .unwrap();

                    res
                },
            )
            .await; // submit

        tracing::info!("run handler submitted");
        let result = result_future.await?; //.expect("ok");
        tracing::info!("run handler done");
        Ok(result)
    }
}

async fn make_wasm_instance(path: &PathBuf) -> Result<WasmInstance> {
    // there's lots of interesting timeout and queueing stuff in
    // https://github.com/bytecodealliance/wasmtime/blob/7948e0ff623ec490ab3579a1f068ac10647cb578/crates/wasi-http/src/handler.rs
    // it'd be nice to (not) replicate that?

    let mut db_path = path.clone();
    db_path.set_extension("sqlite3");

    let sqlite = rusqlite::Connection::open(db_path)?;
    sqlite.execute(
        "CREATE TABLE IF NOT EXISTS test (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) strict",
        [],
    )?;

    let count: i64 = sqlite.query_row("SELECT COUNT(*) FROM test", [], |row| row.get(0))?;
    if count == 0 {
        sqlite.execute(
            "INSERT INTO test (key, value) VALUES (?, ?)",
            ["hello", "world"],
        )?;
        sqlite.execute(
            "INSERT INTO test (key, value) VALUES (?, ?)",
            ["how", "are you"],
        )?;
    }

    let mut config = Config::new();
    config.async_support(true);
    config.wasm_component_model_async(true);
    config.strategy(wasmtime::Strategy::Cranelift);
    let engine = Engine::new(&config)?;

    let mut linker = Linker::<MyState>::new(&engine);

    // which wasi p's do we add here? is the compatibility, somehow?
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi::p3::add_to_linker(&mut linker)?;
    wasmtime_wasi_http::p3::add_to_linker(&mut linker)?;

    sqlite::sqlite_add_to_linker(&mut linker)?;

    // reasonableness:
    // - http timeouts? aborting? max concurrency?
    // - error wrapping/converting for sqlite?
    // - multiple sqlite connections / pool?

    tracing::info!("loading component");

    let component = wasmtime::component::Component::from_file(&engine, path)?;

    tracing::info!("instantiating component");

    let ctx = WasiCtx::builder().inherit_stdout().inherit_stderr().build();

    let mut store = Store::new(
        &engine,
        MyState {
            ctx: ctx,
            sqlite: sqlite::SqliteCtx::new(sqlite),
            http: Default::default(),
            table: Default::default(),
        },
    );

    let proxy: generated::App =
        generated::App::instantiate_async(&mut store, &component, &linker).await?;

    tracing::info!("starting running");

    let (sender, worker) = wasmtimewrapper::make_worker(store, proxy);

    tokio::task::spawn(async move { worker.run().await });

    let instance = WasmInstance { sender: sender };

    instance.initialize().await?;

    Ok(instance)
}

async fn index_handler(server: Extension<Arc<ServerState>>) -> impl axum::response::IntoResponse {
    let mut apps: Vec<String> = server.instances.lock().keys().cloned().collect();
    apps.sort();
    maud::html! {
        html {
            body {
                @for app in apps {
                    a href=(format!("/{}/", app)) { "Check out " (app) }
                    br {}
                }
            }
        }
    }
}

async fn run_handler(
    server: Extension<Arc<ServerState>>,
    mut req: http::Request<axum::body::Body>,
) -> axum::response::Response {
    tracing::info!("run handler start");
    let mut uri = req.uri().clone().into_parts();

    let Some(pq) = uri.path_and_query else {
        return "bad path".into_response();
    };

    let path = &pq.path()[1..];
    let query = pq.query().unwrap_or("");

    let Some(slash) = path.find("/") else {
        return "bad path".into_response();
    };

    let app_name = &path[..slash];
    let path: String = path[slash..].into();

    let instance = server.instances.lock().get(app_name).expect("good").clone();

    let new_path = format!("{}{}", path, query);
    uri.path_and_query = Some(new_path.parse().expect("huh"));

    let uri = http::Uri::from_parts(uri).expect("huh");

    tracing::info!("from: {} to: {}", req.uri(), uri);
    *req.uri_mut() = uri;

    let resp = instance.handle(req).await.expect("huh");
    resp.into_response()
}

fn make_server(server: Arc<ServerState>) -> Result<axum::Router> {
    let app = axum::Router::new()
        .route("/", axum::routing::get(index_handler))
        .route("/{app}/", axum::routing::get(run_handler))
        .route("/{app}/{*rest}", axum::routing::get(run_handler))
        .layer(Extension(server));
    Ok(app)
}

async fn run_server(router: axum::Router) -> Result<()> {
    tracing::info!("running server");
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("listener started");
    axum::serve(listener, router).await?;
    tracing::info!("listener started");
    Ok(())
}

async fn reload_apps() -> Result<HashMap<String, (PathBuf, SystemTime)>> {
    let mut map = HashMap::new();

    let mut entries = tokio::fs::read_dir("apps").await?;

    loop {
        let Some(entry) = entries.next_entry().await? else {
            break;
        };

        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(name) = name.strip_suffix(".wasm") else {
            continue;
        };

        let metadata = entry.metadata().await?;
        let Ok(time) = metadata.modified() else {
            continue;
        };

        map.insert(name.into(), (entry.path(), time));
    }

    Ok(map)
}

async fn sync_apps(stop: CancellationToken, state: Arc<ServerState>) -> Result<()> {
    let mut previous: HashMap<String, (PathBuf, SystemTime)> = HashMap::new();

    loop {
        let current = reload_apps().await?;

        let mut to_remove = vec![];
        for (key, _) in previous.iter() {
            if !current.contains_key(key) {
                // to remove. do it now or plan it?
                tracing::info!("removing {}", key);
                to_remove.push(key.clone());
            }
        }
        for key in to_remove {
            previous.remove(&key);
            {
                let mut guard = state.instances.lock();
                guard.remove(&key);
            }
        }

        for (key, value) in current.iter() {
            if let Some(existing) = previous.get(key) {
                if existing == value {
                    // ok!
                } else {
                    // update
                    tracing::info!("reloading {}", key);
                    previous.insert(key.clone(), value.clone());

                    // TODO: handle errors...
                    let instance = make_wasm_instance(&value.0).await.unwrap();
                    {
                        // TODO: completely stop existing instance

                        let mut guard = state.instances.lock();
                        guard.insert(key.clone(), Arc::new(instance));
                    }
                }
            } else {
                // load first time.
                tracing::info!("loading {}", key);
                previous.insert(key.clone(), value.clone());

                // TODO: handle errors...
                let instance = make_wasm_instance(&value.0).await.unwrap();
                {
                    let mut guard = state.instances.lock();
                    guard.insert(key.clone(), Arc::new(instance));
                }
            }
        }

        // TODO: announce that we have now loaded for the first time? or something like that?

        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {
                // good
            }
            _ = stop.cancelled() => {
                break
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("help");

    println!("Hello, world!");

    let instances = HashMap::new();

    let server = Arc::new(ServerState {
        instances: Mutex::new(instances),
    });

    {
        let server = server.clone();
        tokio::task::spawn(async move {
            sync_apps(CancellationToken::new(), server).await.unwrap();
        });
    }

    let router = make_server(server.clone())?;
    // run_server(router).await?;
    tunnel::run_client(
        "/Users/jelle/hack/pos2/tunnel/testing/client".into(),
        router,
    )
    .await?;

    Ok(())
}

struct MyState {
    ctx: WasiCtx,
    sqlite: sqlite::SqliteCtx,
    http: DefaultWasiHttpCtx,
    table: ResourceTable,
}

impl WasiView for MyState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for MyState {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
        }
    }
}

impl sqlite::SqliteView for MyState {
    fn sqlite(&mut self) -> sqlite::SqliteCtxView<'_> {
        sqlite::SqliteCtxView {
            ctx: &mut self.sqlite,
            table: &mut self.table,
        }
    }
}
