use anyhow::Context as _;
use wasmtime::component::TaskExit;
use wasmtime::component::Accessor;
use wasmtime::Result;
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p3::bindings::http::types::Response;
use wasmtime_wasi_http::p3::{Request, WasiHttpView};

wasmtime::component::bindgen!({
    world: "jelle:test/app",
    path: "../wit-new",
    with: {
        "wasi": wasmtime_wasi::p3::bindings,
        "wasi:http": wasmtime_wasi_http::p3::bindings::http,
        "jelle:test/sqlite/transaction": crate::sqlite::Transaction,
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
