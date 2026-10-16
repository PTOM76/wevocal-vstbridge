#!/bin/sh
# wasm をビルドし、Node 向け（pkg/）とブラウザ向け（pkg-web/）の JS を作る。
# 要るもの: rustup target add wasm32-unknown-unknown、cargo install wasm-bindgen-cli --version 0.2.129（Cargo.toml の wasm-bindgen と同じ版）
set -e
cargo build --target wasm32-unknown-unknown --release --lib
wasm-bindgen target/wasm32-unknown-unknown/release/vstbridge.wasm --out-dir pkg --target nodejs
wasm-bindgen target/wasm32-unknown-unknown/release/vstbridge.wasm --out-dir pkg-web --target web
echo "pkg/（Node）と pkg-web/（ブラウザ）を作った"
