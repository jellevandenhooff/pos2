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

mod bindings {
    wit_bindgen::generate!({
        world: "jelle:test/app",
        path: "../wit-new",
        with: {
            // TODO: wish there was some wildcard support?
            "wasi:random/random@0.3.0-rc-2025-09-16": wasip3::random::random,
            "wasi:cli/types@0.3.0-rc-2025-09-16": wasip3::cli::types,
            "wasi:cli/stdout@0.3.0-rc-2025-09-16": wasip3::cli::stdout,
            "wasi:cli/stderr@0.3.0-rc-2025-09-16": wasip3::cli::stderr,
            "wasi:cli/stdin@0.3.0-rc-2025-09-16": wasip3::cli::stdin,
            "wasi:clocks/types@0.3.0-rc-2025-09-16": wasip3::clocks::types,
            "wasi:clocks/monotonic-clock@0.3.0-rc-2025-09-16": wasip3::clocks::monotonic_clock,
            "wasi:clocks/wall-clock@0.3.0-rc-2025-09-16": wasip3::clocks::wall_clock,
            "wasi:http/types@0.3.0-rc-2025-09-16": wasip3::http::types,
            "wasi:http/handler@0.3.0-rc-2025-09-16": wasip3::http::handler,
        }
    });
}

bindings::export!(Example with_types_in bindings);

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

    async_handler(value)
}

async fn templated() -> impl axum::response::IntoResponse {
    let value = {
        let mut guard = STATE.lock().await;
        *guard += 1;
        *guard
    };

    maud::html!(
        head {
            title { "hello" }
        }
        body {
            "current value: " (value)
        }
    )
}

async fn handler(
    request: wasip3_http_wrapper::IncomingRequest,
) -> impl wasip3_http_wrapper::IntoResponse {
    Router::new()
        .route("/", get(root))
        .route("/templated", get(templated))
        .call(request)
        .await
}

// impl exports::jelle::test::app::Guest for Example {}
//
impl bindings::Guest for Example {
    async fn initialize() -> Result<(), ()> {
        // show that we can do something in the background?
        println!("hello?");
        wit_bindgen::spawn(async move {
            for i in 0..100 {
                println!("tick {i}");
                wait_for(100_000_000).await; // 100ms
            }
        });

        use bindings::jelle::test::sqlite;

        let changed = sqlite::execute(
            "INSERT INTO test( key, value) VALUES (?, ?)".into(),
            vec![
                sqlite::Value::StringValue("from".into()),
                sqlite::Value::StringValue("wasm".into()),
            ],
        )
        .await
        .expect("huh");
        println!("changed: {changed}");

        {
            let tx = sqlite::begin().await.expect("begin");

            let _changed = tx
                .execute(
                    "INSERT INTO test( key, value) VALUES (?, ?)".into(),
                    vec![
                        sqlite::Value::StringValue("inside".into()),
                        sqlite::Value::StringValue("tx".into()),
                    ],
                )
                .await
                .expect("huh");

            let rows = tx
                .query("SELECT * FROM test".into(), vec![])
                .await
                .expect("huh");

            for row in rows {
                println!("{:?}", row);
            }

            // tx.commit().await.expect("commit");
            tx.rollback().await.expect("rollback");
        }

        let rows = sqlite::query("SELECT * FROM test".into(), vec![])
            .await
            .expect("huh");

        for row in rows {
            println!("{:?}", row);
        }

        wait_for(500_000_000).await; // 500ms

        println!("initialize about to return");
        Ok(())
    }
}

// TODO: can this refer to the existing wasi type?
impl bindings::exports::wasi::http::handler::Guest for Example {
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
