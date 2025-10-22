use futures::stream::StreamExt;
use futures_core::stream::Stream;
use http_body_util::combinators::BoxBody;
use std::convert::Infallible;
use std::ops::Deref;
use std::sync::Arc;

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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

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

    // questions:
    // - initialize component how/where?
    // - streaming response(s)?
    // - timeouts? aborting? max concurrency?
    //
    // - if I want to reuse some generated wit bindings, can I? ah, yes, there's a "with" option in bindgen
    //
    // what would be nice to make...
    // - format html with maud?
    // - wit bindgen for:
    //   - sqlite storage
    //   - init function

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
            // println!("res {res:?}");

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

#[derive(Default)]
struct MyState {
    ctx: WasiCtx,
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
