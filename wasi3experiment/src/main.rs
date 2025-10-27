use anyhow::bail;
use axum::Extension;
use axum::handler::{Handler, HandlerWithoutStateExt};
use http_body_util::BodyExt;
use std::sync::Arc;
use wasmtime::component::{Accessor, Linker, ResourceTable};
use wasmtime::{Config, Engine, Result, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p3::{DefaultWasiHttpCtx, Request, WasiHttpCtxView, WasiHttpView};

mod generated;
mod wasmtimewrapper;

mod sqlite;

async fn simple_handler() -> impl axum::response::IntoResponse {
    "huh"
}

async fn run_handler(
    ext: Extension<Arc<wasmtimewrapper::WorkSender<MyState, generated::App>>>,
    req: http::Request<axum::body::Body>,
) -> impl axum::response::IntoResponse {
    tracing::info!("run handler start");
    let result_future = ext
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
    let result = result_future.await.expect("ok");
    tracing::info!("run handler done");
    result
    // String::from_utf8(result).unwrap()
    // "hello, world"
}

async fn run_server(wt: Arc<wasmtimewrapper::WorkSender<MyState, generated::App>>) -> Result<()> {
    // drop(wt);
    tracing::info!("running server");
    // let app = axum::Router::new()
    // .route("/", axum::routing::get(run_handler))
    // .route("/{*rest}", axum::routing::get(run_handler))
    // .route("/", axum::routing::get(simple_handler))
    // .layer(Extension(wt))
    /*
    .layer(axum::middleware::from_fn_with_state(
        state.clone(),
        auth_middleware,
    ))
    .layer(Extension(Arc::new(Registry::new()?)))
    .with_state(state);
    */
    let app = run_handler.layer(Extension(wt));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("listener started");
    // tokio::spawn((async move || -> Result<()> {
    axum::serve(listener, app.into_service()).await?;
    tracing::info!("listener started");
    // Ok(())
    // })());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

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

    println!("Hello, world!");

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
    //
    // what would be nice to make...
    // - hide clunky stuff behind something

    tracing::info!("loading component");

    let component = wasmtime::component::Component::from_file(
        &engine,
        "../target/wasm32-wasip2/debug/wasi3app.wasm",
    )?;

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

    // there's lots of interesting timeout and queueing stuff in
    // https://github.com/bytecodealliance/wasmtime/blob/7948e0ff623ec490ab3579a1f068ac10647cb578/crates/wasi-http/src/handler.rs
    // it'd be nice to (not) replicate that?

    let (sender, worker) = wasmtimewrapper::make_worker(store, proxy);

    tokio::task::spawn(async move { worker.run().await });

    sender
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

    run_server(Arc::new(sender)).await?;

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
