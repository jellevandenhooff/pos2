use wasip3::clocks::monotonic_clock::wait_for;
use wasip3::http::types::{ErrorCode, Fields, Request, Response};
use wasip3::{wit_bindgen, wit_future, wit_stream};

wasip3::http::proxy::export!(Example);

use lazy_static::lazy_static;

struct Example {}

lazy_static! {
    static ref STATE: tokio::sync::Mutex<u64> = tokio::sync::Mutex::new(0u64);
}

impl wasip3::exports::http::handler::Guest for Example {
    async fn handle(_request: Request) -> Result<Response, ErrorCode> {
        let value = {
            let mut guard = STATE.lock().await;
            *guard += 1;
            *guard
        };

        let (mut body_tx, body_rx) = wit_stream::new();
        let (body_result_tx, body_result_rx) = wit_future::new(|| Ok(None));
        let (response, _future_result) =
            Response::new(Fields::new(), Some(body_rx), body_result_rx);
        drop(body_result_tx);

        wit_bindgen::spawn(async move {
            let remaining = body_tx
                .write_all(format!("Hello, WASI! {}", value).into_bytes().to_vec())
                .await;
            assert!(remaining.is_empty());
            wait_for(1_000_000_000).await; // 1s

            let remaining = body_tx.write_all(b"Hello, again, WASI!".to_vec()).await;
            assert!(remaining.is_empty());
        });
        Ok(response)
    }
}
