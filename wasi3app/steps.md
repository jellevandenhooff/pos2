rustup target add wasm32-wasip2 # should this be wasip1? it is in https://www.fermyon.com/blog/looking-ahead-to-wasip3
cargo install wasm-tools # can this be a binary dep?
cargo build --target=wasm32-wasip2


