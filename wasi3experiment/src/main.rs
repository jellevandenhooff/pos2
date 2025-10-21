use futures::stream::StreamExt;
use futures_core::stream::Stream;
use http_body_util::combinators::BoxBody;
use std::convert::Infallible;
use std::sync::Arc;

use http::{HeaderMap, Method};
use http_body_util::BodyExt;
use wasmtime::component::{Accessor, AsAccessor, Linker, ResourceTable};
use wasmtime::{AsContext, Config, Engine, Result, Store, StoreContextMut};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p3::{DefaultWasiHttpCtx, Request, WasiHttpCtxView, WasiHttpView};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    println!("Hello, world!");

    let mut config = Config::new();
    config.async_support(true);
    config.wasm_component_model_async(true);
    let engine = Engine::new(&config)?;

    let mut linker = Linker::<MyState>::new(&engine);
    // unfortunate... adding this because of compile target. maybe not so bad?
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi::p3::add_to_linker(&mut linker)?;
    wasmtime_wasi_http::p3::add_to_linker(&mut linker)?;
    // ... add any further functionality to `linker` if desired ...

    // questions:
    // - initialize component how/where?
    // - streaming response(s)?
    // - timeouts? aborting? max concurrency?
    //
    // what would be nice to make...
    // - tiny axum http router server (https://github.com/fermyon/spin-wasip3-http)
    // - format html with maud?
    // - sqlite storage
    // - init function

    let component = wasmtime::component::Component::from_file(
        &engine,
        "../target/wasm32-wasip2/debug/wasi3app.wasm",
    )?;

    let mut store = Store::new(&engine, MyState::default());
    let proxy =
        wasmtime_wasi_http::p3::bindings::Proxy::instantiate_async(&mut store, &component, &linker)
            .await?;

    // there's lots of interesting timeout and queueing stuff in
    // https://github.com/bytecodealliance/wasmtime/blob/7948e0ff623ec490ab3579a1f068ac10647cb578/crates/wasi-http/src/handler.rs
    // it'd be nice to (not) replicate that?

    // how to abstract away this api? async req -> resp, ideally?

    store
        .run_concurrent(async move |store| -> Result<_> {
            let mut futs = vec![];

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
