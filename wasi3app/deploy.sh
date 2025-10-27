#!/bin/bash

set -ex

cargo build --target=wasm32-wasip2 --release
cp ../target/wasm32-wasip2/release/wasi3app.wasm ../wasi3experiment/apps/$1.wasm
