use axum::Extension;
use axum::handler::{Handler, HandlerWithoutStateExt};
use futures::stream::StreamExt;
use http_body_util::BodyExt;
use std::sync::Arc;
use wasmtime::component::{Accessor, Linker, Resource, ResourceTable};
use wasmtime::{Config, Engine, Result, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p3::{DefaultWasiHttpCtx, Request, WasiHttpCtxView, WasiHttpView};

use anyhow::bail;

pub struct Transaction {
    // transaction: rusqlite::Transaction<'static>,
    running: bool,
}

mod generated {
    wasmtime::component::bindgen!({
        world: "jelle:test/app",
        path: "../wit-new",
        with: {
            "wasi": wasmtime_wasi::p3::bindings,
            "wasi:http": wasmtime_wasi_http::p3::bindings::http,
            "jelle:test/sqlite/transaction": super::Transaction,
        },

        imports: {
            "jelle:test/sqlite/[drop]transaction": async | store | trappable | tracing,
            default: trappable | tracing,
        },

        // TODO: do not know what these mean...?
        exports: { default: async | store | task_exit },
    });

    // TODO: this is copied from wasmtime_wasi_http... can we somehow
    // cast into their type???
    use anyhow::Context as _;
    use wasmtime::component::{Accessor, TaskExit};
    use wasmtime_wasi_http::p3::WasiHttpView;
    use wasmtime_wasi_http::p3::bindings::http::types::{ErrorCode, Request, Response};

    impl App {
        /// Call `wasi:http/handler#handle` on [Proxy] getting a [Response] back.
        pub async fn handle(
            &self,
            store: &Accessor<impl WasiHttpView>,
            req: impl Into<Request>,
        ) -> wasmtime::Result<Result<(Response, TaskExit), ErrorCode>> {
            let req = store.with(|mut store| {
                store
                    .data_mut()
                    .http()
                    .table
                    .push(req.into())
                    .context("failed to push request to table")
            })?;

            match self.wasi_http_handler().call_handle(store, req).await? {
                (Ok(res), task) => {
                    let res = store.with(|mut store| {
                        store
                            .data_mut()
                            .http()
                            .table
                            .delete(res)
                            .context("failed to delete response from table")
                    })?;
                    Ok(Ok((res, task)))
                }
                (Err(err), _) => Ok(Err(err)),
            }
        }
    }
}

struct SqliteCtx {
    conn: rusqlite::Connection,
    in_tx: bool,
}

impl wasmtime::component::HasData for Sqlite {
    type Data<'a> = SqliteCtxView<'a>;
}

struct Sqlite;

use generated::jelle::test::sqlite;

impl sqlite::Host for SqliteCtxView<'_> {}
impl sqlite::HostTransaction for SqliteCtxView<'_> {}

trait SqliteView: Send {
    fn sqlite(&mut self) -> SqliteCtxView<'_>;
}

struct SqliteCtxView<'a> {
    pub ctx: &'a mut SqliteCtx,
    pub table: &'a mut ResourceTable,
}

impl SqliteView for MyState {
    fn sqlite(&mut self) -> SqliteCtxView<'_> {
        SqliteCtxView {
            ctx: &mut self.sqlite,
            table: &mut self.table,
        }
    }
}

pub fn sqlite_add_to_linker<T>(linker: &mut Linker<T>) -> wasmtime::Result<()>
where
    T: SqliteView + 'static,
{
    sqlite::add_to_linker::<_, Sqlite>(linker, T::sqlite)?;
    Ok(())
}

impl From<rusqlite::Error> for sqlite::ErrorCode {
    fn from(value: rusqlite::Error) -> Self {
        sqlite::ErrorCode::Bad
    }
}

use rusqlite::types::FromSqlResult;
use rusqlite::types::ToSqlOutput;
use rusqlite::types::ValueRef;

impl rusqlite::ToSql for sqlite::Value {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        match &self {
            &sqlite::Value::StringValue(v) => {
                Ok(ToSqlOutput::Borrowed(ValueRef::Text(v.as_bytes())))
            }
            &sqlite::Value::S64Value(v) => Ok(ToSqlOutput::Borrowed(ValueRef::Integer(*v))),
            &sqlite::Value::F64Value(v) => Ok(ToSqlOutput::Borrowed(ValueRef::Real(*v))),
            &sqlite::Value::NullValue => Ok(ToSqlOutput::Borrowed(ValueRef::Null)),
            &sqlite::Value::BlobValue(v) => Ok(ToSqlOutput::Borrowed(ValueRef::Blob(v))),
        }
    }
}

impl rusqlite::types::FromSql for sqlite::Value {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Text(s) => Ok(sqlite::Value::StringValue(
                str::from_utf8(s).expect("wtf").into(),
            )),
            ValueRef::Integer(v) => Ok(sqlite::Value::S64Value(v)),
            ValueRef::Real(v) => Ok(sqlite::Value::F64Value(v)),
            ValueRef::Blob(v) => Ok(sqlite::Value::BlobValue(v.into())),
            ValueRef::Null => Ok(sqlite::Value::NullValue),
        }
    }
}

impl sqlite::HostTransactionWithStore for Sqlite {
    async fn query<T>(
        accessor: &Accessor<T, Self>,
        self_: Resource<sqlite::Transaction>,
        query: String,
        args: Vec<sqlite::Value>,
    ) -> wasmtime::Result<std::result::Result<Vec<sqlite::Row>, sqlite::ErrorCode>> {
        // TODO: deduplicate
        accessor.with(|mut store| {
            let view = store.get();
            let ctx = view.ctx;
            let tx = view.table.get_mut(&self_)?;
            if !tx.running {
                return Ok(Err(sqlite::ErrorCode::Bad));
            }
            let mut prepared = ctx.conn.prepare(&query)?;
            let count = prepared.column_count(); // TODO: this count may be out of date???????
            let mut rows_iter = prepared.query(rusqlite::params_from_iter(args))?;
            let mut rows = vec![];

            loop {
                let row_accessor = match rows_iter.next()? {
                    None => break,
                    Some(row) => row,
                };
                let mut row = vec![];
                for idx in 0..count {
                    row.push(row_accessor.get(idx)?);
                }
                rows.push(row);
            }

            Ok(Ok(rows))
        })
    }

    async fn execute<T>(
        accessor: &Accessor<T, Self>,
        self_: Resource<sqlite::Transaction>,
        query: String,
        args: Vec<sqlite::Value>,
    ) -> wasmtime::Result<std::result::Result<u64, sqlite::ErrorCode>> {
        accessor.with(|mut store| {
            let view = store.get();
            let ctx = view.ctx;
            let tx = view.table.get_mut(&self_)?;
            if !tx.running {
                return Ok(Err(sqlite::ErrorCode::Bad));
            }
            let result = ctx.conn.execute(&query, rusqlite::params_from_iter(args))?;
            Ok(Ok(result as u64))
        })
        // todo!("help");
    }

    async fn commit<T>(
        accessor: &Accessor<T, Self>,
        self_: Resource<sqlite::Transaction>,
    ) -> wasmtime::Result<std::result::Result<(), sqlite::ErrorCode>> {
        tracing::info!("commit");
        accessor.with(|mut store| {
            let view = store.get();
            let ctx = view.ctx;
            let tx = view.table.get_mut(&self_)?;
            if !tx.running {
                return Ok(Err(sqlite::ErrorCode::Bad));
            }
            tx.running = false;
            ctx.in_tx = false;
            ctx.conn.execute("COMMIT", [])?;
            Ok(Ok(()))
        })
    }

    async fn rollback<T>(
        accessor: &Accessor<T, Self>,
        self_: Resource<sqlite::Transaction>,
    ) -> wasmtime::Result<std::result::Result<(), sqlite::ErrorCode>> {
        tracing::info!("rollback");
        accessor.with(|mut store| {
            let view = store.get();
            let ctx = view.ctx;
            let tx = view.table.get_mut(&self_)?;
            if !tx.running {
                return Ok(Err(sqlite::ErrorCode::Bad));
            }
            tx.running = false;
            ctx.in_tx = false;
            ctx.conn.execute("ROLLBACK", [])?;
            Ok(Ok(()))
        })
    }

    async fn drop<T>(
        accessor: &Accessor<T, Self>,
        self_: wasmtime::component::Resource<sqlite::Transaction>,
    ) -> wasmtime::Result<()> {
        tracing::info!("dropping");
        accessor.with(|mut store| {
            let view = store.get();
            let ctx = view.ctx;
            let mut tx = view.table.delete(self_)?;
            if tx.running {
                tx.running = false;
                ctx.in_tx = false;
                ctx.conn.execute("ROLLBACK", [])?;
            }
            drop(tx);
            Ok(())
        })
    }
}

impl sqlite::HostWithStore for Sqlite {
    // TODO: the errors in the http package seem nicer? (it's only one layer of error, somehow?)

    async fn begin<T>(
        accessor: &Accessor<T, Self>,
    ) -> wasmtime::Result<Result<Resource<sqlite::Transaction>, sqlite::ErrorCode>> {
        tracing::info!("begin");

        accessor.with(|mut store| {
            let ctx = store.get().ctx;
            if ctx.in_tx {
                return Ok(Err(sqlite::ErrorCode::Bad));
            }
            ctx.conn.execute("BEGIN", [])?;
            ctx.in_tx = true;
            let resource = store
                .get()
                .table
                .push(sqlite::Transaction { running: true })?;
            Ok(Ok(Resource::new_own(resource.rep())))
        })
    }

    async fn query<T>(
        accessor: &Accessor<T, Self>,
        query: String,
        args: Vec<sqlite::Value>,
    ) -> wasmtime::Result<Result<Vec<sqlite::Row>, sqlite::ErrorCode>> {
        accessor.with(|mut store| {
            let ctx = store.get().ctx;
            if ctx.in_tx {
                return Ok(Err(sqlite::ErrorCode::Bad));
            }
            let mut prepared = ctx.conn.prepare(&query)?;
            let count = prepared.column_count(); // TODO: this count may be out of date???????
            let mut rows_iter = prepared.query(rusqlite::params_from_iter(args))?;
            let mut rows = vec![];

            loop {
                let row_accessor = match rows_iter.next()? {
                    None => break,
                    Some(row) => row,
                };
                let mut row = vec![];
                for idx in 0..count {
                    row.push(row_accessor.get(idx)?);
                }
                rows.push(row);
            }

            Ok(Ok(rows))
        })
    }

    async fn execute<T>(
        accessor: &Accessor<T, Self>,
        query: String,
        args: Vec<sqlite::Value>,
    ) -> wasmtime::Result<Result<u64, sqlite::ErrorCode>> {
        accessor.with(|mut store| {
            let ctx = store.get().ctx;
            if ctx.in_tx {
                return Ok(Err(sqlite::ErrorCode::Bad));
            }
            let result = ctx.conn.execute(&query, rusqlite::params_from_iter(args))?;
            Ok(Ok(result as u64))
        })
    }
}

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

                let (res, task) = proxy.handle(store, request).await.unwrap().unwrap();

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

mod wasmtimewrapper;

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

    sqlite_add_to_linker(&mut linker)?;

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
            sqlite: SqliteCtx {
                conn: sqlite,
                in_tx: false,
            },
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
    sqlite: SqliteCtx,
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
