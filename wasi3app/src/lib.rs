use std::convert::Infallible;

use axum::body::Body;
use wasip3::clocks::monotonic_clock::wait_for;
use wasip3::http::types::{ErrorCode, Request, Response};
use wasip3::wit_bindgen;

use axum::{Router, http::StatusCode, routing::get};
use tower_service::Service;

use bytes::Bytes;
use http_body_util::StreamBody;
use hyper::body::Frame;

wasip3::http::proxy::export!(Example);

mod wasip3_http_wrapper;

use lazy_static::lazy_static;

struct Example {}

lazy_static! {
    static ref STATE: tokio::sync::Mutex<u64> = tokio::sync::Mutex::new(0u64);
}

fn async_handler(counter: u64) -> impl axum::response::IntoResponse {
    type Data = Result<Frame<Bytes>, Infallible>;
    let (tx, rx) = tokio::sync::mpsc::channel::<Data>(2);

    // can't use axum-streams yet because it pulls in full axum (and then tokio)

    wit_bindgen::spawn(async move {
        tx.send(Ok(Frame::data(format!("hello... {counter}").into())))
            .await
            .unwrap();

        // can't use tokio sleep yet because it does not use wasip3
        wait_for(1_000_000_000).await; // 1s

        tx.send(Ok(Frame::data(format!("bye... {counter}").into())))
            .await
            .unwrap();
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = StreamBody::new(stream);

    (StatusCode::OK, Body::new(body))
}

async fn root() -> impl axum::response::IntoResponse {
    let value = {
        let mut guard = STATE.lock().await;
        *guard += 1;
        *guard
    };

    // TODO: sqlite?

    async_handler(value)
}

async fn handler(
    request: wasip3_http_wrapper::IncomingRequest,
) -> impl wasip3_http_wrapper::IntoResponse {
    Router::new().route("/", get(root)).call(request).await
}

impl wasip3::exports::http::handler::Guest for Example {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let request = <wasip3_http_wrapper::IncomingRequest as wasip3_http_wrapper::FromRequest>::from_request(request)?;
        wasip3_http_wrapper::IntoResponse::into_response(handler(request).await)
    }
}

/*
impl wasip3::exports::http::handler::Guest for Example {
    async fn handle(_request: Request) -> Result<Response, ErrorCode> {
        let value = {
            let mut guard = STATE.lock().await;
            *guard += 1;
            *guard
        };

        let (mut body_tx, body_rx) = wit_stream::new();
        let (trailers_tx, trailers_rx) = wit_future::new(|| Ok(None));
        let (response, _response_sent_result_future) =
            Response::new(Fields::new(), Some(body_rx), trailers_rx);
        drop(trailers_tx);

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
*/
