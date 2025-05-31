#!/bin/sh
set -ex
componentize-py -d ../wit componentize --stub-wasi app -o app.wasm
