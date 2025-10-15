#!/bin/bash
set -ex
cargo build
sqlite3 -cmd '.load ./target/debug/libextension.dylib' -cmd '.open foo.db'
