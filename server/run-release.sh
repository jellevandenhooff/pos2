#!/bin/sh
set -ex
cargo run --release -- "$@"
