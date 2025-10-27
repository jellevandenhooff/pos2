use std::convert::Infallible;

use axum::body::Body;
use tokio::sync::oneshot;
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

use futures::Stream;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
struct MyTestStructure {
    some_test_field: String,
}

fn my_source_stream(counter: u64) -> impl Stream<Item = MyTestStructure> {
    let (tx, rx) = tokio::sync::mpsc::channel::<MyTestStructure>(2);

    wit_bindgen::spawn(async move {
        tx.send(MyTestStructure {
            some_test_field: format!("hello... {counter}").into(),
        })
        .await
        .unwrap();

        // can't use tokio sleep yet because it does not use wasip3
        wait_for(1_000_000_000).await; // 1s

        tx.send(MyTestStructure {
            some_test_field: format!("bye... {counter}").into(),
        })
        .await
        .unwrap();
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    stream
}

fn async_handler(counter: u64) -> impl axum::response::IntoResponse {
    (
        [(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        )],
        axum_streams::StreamBodyAs::json_nl(my_source_stream(counter)),
    )
}

async fn stream() -> impl axum::response::IntoResponse {
    let value = {
        let mut guard = STATE.lock().await;
        *guard += 1;
        *guard
    };

    async_handler(value)
}

#[axum::debug_handler]
async fn templated() -> impl axum::response::IntoResponse {
    let tx = sqlite::begin().await.expect("huh");

    let _ = tx
        .execute("UPDATE counter SET value = value + 1".into(), vec![])
        .await
        .expect("huh");

    let rows = tx
        .query("SELECT value FROM counter".into(), vec![])
        .await
        .expect("huh");

    tx.commit().await.expect("huh");

    let value: i64 = rows.get(0).unwrap().get(0).unwrap().try_into().unwrap();

    maud::html!(
        head {
            title { "hello" }
        }
        body {
            p { "hello there" }
            p { "current value: " (value) }
        }
    )
}

async fn handler(
    request: wasip3_http_wrapper::IncomingRequest,
) -> impl wasip3_http_wrapper::IntoResponse {
    Router::new()
        .route("/", get(templated))
        .route("/stream", get(stream))
        .call(request)
        .await
}

use bindings::jelle::test::sqlite;

impl Into<sqlite::Value> for &str {
    fn into(self) -> sqlite::Value {
        sqlite::Value::StringValue(self.into())
    }
}

impl TryInto<String> for &sqlite::Value {
    type Error = anyhow::Error; // TODO: real error
    fn try_into(self) -> Result<String, Self::Error> {
        match self {
            // TODO: it's too bad this copies... right now we use vec.get(0) which borrows
            sqlite::Value::StringValue(s) => Ok(s.into()),
            _ => anyhow::bail!("bad"), // TODO: real error
        }
    }
}

impl TryInto<i64> for &sqlite::Value {
    type Error = anyhow::Error; // TODO: real error
    fn try_into(self) -> Result<i64, Self::Error> {
        match self {
            // TODO: it's too bad this copies... right now we use vec.get(0) which borrows
            sqlite::Value::S64Value(s) => Ok(*s),
            _ => anyhow::bail!("bad"), // TODO: real error
        }
    }
}

// impl exports::jelle::test::app::Guest for Example {}
//
impl bindings::Guest for Example {
    async fn initialize() -> Result<(), ()> {
        // show that we can do something in the background?
        println!("hello?");
        /*
        wit_bindgen::spawn(async move {
            for i in 0..100 {
                println!("tick {i}");
                wait_for(100_000_000).await; // 100ms
            }
        });
        */

        let _ = sqlite::execute(
            "CREATE TABLE counter (value INTEGER NOT NULL) STRICT".into(),
            vec![],
        )
        .await
        .expect("huh");

        let _ = sqlite::execute("INSERT INTO counter (value) VALUES (0)".into(), vec![])
            .await
            .expect("huh");

        let changed = sqlite::execute(
            "INSERT INTO test (key, value) VALUES (?, ?)".into(),
            vec!["from".into(), "wasm".into()],
        )
        .await
        .expect("huh");
        println!("changed: {changed}");

        {
            let tx = sqlite::begin().await.expect("begin");

            let _changed = tx
                .execute(
                    "INSERT INTO test( key, value) VALUES (?, ?)".into(),
                    vec!["inside".into(), "tx".into()],
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
            // tx.rollback().await.expect("rollback");
        }

        let rows = sqlite::query("SELECT * FROM test".into(), vec![])
            .await
            .expect("huh");

        for row in rows {
            let key: String = row.get(0).unwrap().try_into().unwrap();
            let value: String = row.get(1).unwrap().try_into().unwrap();
            println!("{key} = {value}");
        }

        // wait_for(500_000_000).await; // 500ms

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
