use futures::stream::StreamExt;
use futures_core::stream::Stream;
use http_body_util::combinators::BoxBody;
use parking_lot::Mutex;
use std::convert::Infallible;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;
use wasmtime::component::types::Case;

use http::{HeaderMap, Method};
use http_body_util::BodyExt;
use wasmtime::component::{Accessor, AsAccessor, Linker, ResourceTable};
use wasmtime::{AsContext, Config, Engine, Result, Store, StoreContextMut};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p3::{DefaultWasiHttpCtx, Request, WasiHttpCtxView, WasiHttpView};

use anyhow::bail;

mod generated {
    wasmtime::component::bindgen!({
        world: "jelle:test/app",
        path: "../wit-new",
        with: {
            "wasi": wasmtime_wasi::p3::bindings,
            "wasi:http": wasmtime_wasi_http::p3::bindings::http,
        },
        // TODO: do not know what these mean...?
        imports: {
            default: trappable | tracing,
        },
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
    conn: Arc<Mutex<rusqlite::Connection>>,
}

impl wasmtime::component::HasData for Sqlite {
    type Data<'a> = &'a mut SqliteCtx;
}

struct Sqlite;

use generated::jelle::test::sqlite;

impl sqlite::Host for SqliteCtx {}

trait SqliteView: Send {
    fn sqlite(&mut self) -> &mut SqliteCtx;
}

impl SqliteView for MyState {
    fn sqlite(&mut self) -> &mut SqliteCtx {
        &mut self.sqlite
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

impl sqlite::HostWithStore for Sqlite {
    async fn query<T>(
        accessor: &wasmtime::component::Accessor<T, Self>,
        query: String,
        args: Vec<sqlite::Value>,
    ) -> wasmtime::Result<Result<Vec<sqlite::Row>, sqlite::ErrorCode>> {
        Ok(accessor.with(|mut store| {
            let guard = store.get().conn.lock();
            let mut prepared = guard.prepare(&query)?;
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

            Ok(rows)
        }))
    }

    async fn execute<T>(
        accessor: &wasmtime::component::Accessor<T, Self>,
        query: String,
        args: Vec<sqlite::Value>,
    ) -> wasmtime::Result<Result<u64, sqlite::ErrorCode>> {
        Ok(accessor.with(|mut store| {
            let guard = store.get().conn.lock();
            let result = guard.execute(&query, rusqlite::params_from_iter(args))?;
            Ok(result as u64)
        }))
    }
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

    sqlite_add_to_linker(&mut linker)?;

    // questions:
    // - timeouts? aborting? max concurrency?
    // - if I want to reuse some generated wit bindings, can I? ah, yes, there's a "with" option in bindgen
    //
    // what would be nice to make...
    // - format html with maud?

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
                conn: Arc::new(Mutex::new(sqlite)),
            },
            http: Default::default(),
            table: Default::default(),
        },
    );

    let proxy = generated::App::instantiate_async(&mut store, &component, &linker).await?;

    /*
        let proxy =
            wasmtime_wasi_http::p3::bindings::Proxy::instantiate_async(&mut store, &component, &linker)
                .await?;
    */

    tracing::info!("starting running");

    // there's lots of interesting timeout and queueing stuff in
    // https://github.com/bytecodealliance/wasmtime/blob/7948e0ff623ec490ab3579a1f068ac10647cb578/crates/wasi-http/src/handler.rs
    // it'd be nice to (not) replicate that?

    // how to abstract away this api? async req -> resp, ideally?

    store
        .run_concurrent(async move |store| -> Result<_> {
            let mut futs = vec![];

            let (result, _task) = proxy.call_initialize(store).await?;
            if let Err(_) = result {
                bail!("failed to start worker");
            }

            for _i in 0..5 {
                use tokio::sync::oneshot;
                let (tx, rx) =
                    oneshot::channel::<http::Response<BoxBody<bytes::Bytes, ErrorCode>>>();

                let body: String = "hello world".into();
                let req = http::Request::new(body);

                let (request, request_io_result) = Request::from_http(req);

                let (res, task) = proxy.handle(store, request).await??;
                let res = store.with(|mut store| res.into_http(&mut store, request_io_result))?;

                // _ = res.map(|body| body.map_err(|e| e.into()).boxed());
                tx.send(res).unwrap();

                tokio::spawn(async move {
                    let resp = rx.await.unwrap();

                    tracing::info!("{:?}", resp);
                    let (parts, body) = resp.into_parts();

                    tracing::info!("{parts:?}");
                    let mut body = body.into_data_stream();

                    loop {
                        let frame = body.next().await;
                        match frame {
                            Some(frame) => {
                                tracing::info!("frame {:?}", frame);
                            }
                            None => break,
                        }
                    }
                });

                futs.push(task.block(store));
            }

            futures::future::join_all(futs).await;

            Ok(())
        })
        .await??;

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
