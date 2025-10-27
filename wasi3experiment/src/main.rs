use anyhow::bail;
use axum::Extension;
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use std::collections::HashMap;
use std::sync::Arc;
use wasmtime::component::{Accessor, Linker, ResourceTable};
use wasmtime::{Config, Engine, Result, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p3::{DefaultWasiHttpCtx, Request, WasiHttpCtxView, WasiHttpView};

mod generated;
mod wasmtimewrapper;

mod sqlite;

struct ServerState {
    instances: HashMap<String, WasmInstance>,
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

async fn make_wasm_instance(path: &str) -> Result<WasmInstance> {
    // there's lots of interesting timeout and queueing stuff in
    // https://github.com/bytecodealliance/wasmtime/blob/7948e0ff623ec490ab3579a1f068ac10647cb578/crates/wasi-http/src/handler.rs
    // it'd be nice to (not) replicate that?

    let sqlite = rusqlite::Connection::open_in_memory()?;
    sqlite.execute(
        "CREATE TABLE test (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) strict",
        [],
    )?;
    sqlite.execute(
        "INSERT INTO test (key, value) VALUES (?, ?)",
        ["hello", "world"],
    )?;

    sqlite.execute(
        "INSERT INTO test (key, value) VALUES (?, ?)",
        ["how", "are you"],
    )?;

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
    let apps: Vec<&String> = server.instances.keys().collect();
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

    let instance = server.instances.get(app_name).expect("good");

    let new_path = format!("{}{}", path, query);
    uri.path_and_query = Some(new_path.parse().expect("huh"));

    let uri = http::Uri::from_parts(uri).expect("huh");

    tracing::info!("from: {} to: {}", req.uri(), uri);
    *req.uri_mut() = uri;

    let resp = instance.handle(req).await.expect("huh");
    resp.into_response()
}

async fn run_server(server: Arc<ServerState>) -> Result<()> {
    tracing::info!("running server");
    let app = axum::Router::new()
        .route("/", axum::routing::get(index_handler))
        .route("/{app}/", axum::routing::get(run_handler))
        .route("/{app}/{*rest}", axum::routing::get(run_handler))
        .layer(Extension(server));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("listener started");
    axum::serve(listener, app).await?;
    tracing::info!("listener started");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    println!("Hello, world!");

    let instance_a = make_wasm_instance("../target/wasm32-wasip2/debug/wasi3app.wasm").await?;
    let instance_b = make_wasm_instance("../target/wasm32-wasip2/debug/wasi3app.wasm").await?;

    let server = ServerState {
        instances: HashMap::from([("a".into(), instance_a), ("b".into(), instance_b)]),
    };

    run_server(Arc::new(server)).await?;

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
