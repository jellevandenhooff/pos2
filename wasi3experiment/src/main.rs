use std::sync::Arc;

use http::{HeaderMap, Method};
use wasmtime::component::{Accessor, Linker, ResourceTable};
use wasmtime::{AsContext, Config, Engine, Result, Store, StoreContextMut};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::p3::{DefaultWasiHttpCtx, Request, WasiHttpCtxView, WasiHttpView};

#[tokio::main]
async fn main() -> Result<()> {
    println!("Hello, world!");

    let mut config = Config::new();
    config.async_support(true);
    config.wasm_component_model_async(true);
    let engine = Engine::new(&config)?;

    let mut linker = Linker::<MyState>::new(&engine);
    wasmtime_wasi::p3::add_to_linker(&mut linker)?;
    wasmtime_wasi_http::p3::add_to_linker(&mut linker)?;
    // ... add any further functionality to `linker` if desired ...

    let mut store = Store::new(&engine, MyState::default());

    let component = wasmtime::component::Component::from_file(
        &engine,
        "../../target/wasm32-wasip2/debug/libwasi3app.rlib",
    )?;

    // let instance = linker.instantiate_async(&mut store, &c).await?;

    let proxy =
        wasmtime_wasi_http::p3::bindings::Proxy::instantiate(&mut store, &component, &linker)?;

    // store.run_concurrent();

    proxy.wasi_http_handler()

    let (request, request_io_result) =
       Request::new(
            Method::GET,
            None,
            None,
            None,
            Arc::new(HeaderMap::new()),
            None,
            "".into(),
        );

 
    let (res, task) = proxy.handle(&store, request).await??;
    // let res = store.with(|mut store| res.into_http(&mut store, request_io_result))?;
    _ = tx.send(res.map(|body| body.map_err(|e| e.into()).boxed()));

    proxy.handle(
        &store,
 ,
    );

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
