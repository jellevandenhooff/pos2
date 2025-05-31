{ pkgs ? import <nixpkgs> {}}:

pkgs.mkShell {
  buildInputs = with pkgs; [
    rustc
    cargo
    rustfmt
    rust-analyzer
    clippy
    pkg-config
    clang
    openssl
    go
  ];

  RUST_BACKTRACE = 1;
}
