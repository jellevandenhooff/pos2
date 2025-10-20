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

    let mut store = Store::new(&engine, MyState::default());

    let component = wasmtime::component::Component::from_file(
        &engine,
        "../target/wasm32-wasip2/debug/wasi3app.wasm",
    )?;

    // let instance = linker.instantiate_async(&mut store, &c).await?;

    let proxy =
        wasmtime_wasi_http::p3::bindings::Proxy::instantiate_async(&mut store, &component, &linker)
            .await?;
    let resp = store
        .run_concurrent(async |store| -> Result<_> {
            // store.run_concurrent();

            // proxy.wasi_http_handler()
            //
            let body: String = "hello world".into();
            let req = http::Request::new(body);

            let (request, request_io_result) = Request::from_http(req);
            let (res, task) = proxy.handle(store, request).await??;
            let res = store.with(|mut store| res.into_http(&mut store, request_io_result))?;
            task.block(store).await;

            // _ = res.map(|body| body.map_err(|e| e.into()).boxed());
            Ok(res)

            // proxy.handle(&store, request);

            // Ok(())
        })
        .await??;

    println!("{:?}", resp);

    // instance.spawn(store, task)?;
    // linker.instantiate(store, component)

    // ... use `linker` to instantiate within `store` ...

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
