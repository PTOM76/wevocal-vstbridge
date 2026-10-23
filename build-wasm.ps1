# wasm をビルドし、Node 向け（pkg/）とブラウザ向け（pkg-web/）の JS を作る（build-wasm.sh の Windows 版）。
# 要るもの: rustup target add wasm32-unknown-unknown、cargo install wasm-bindgen-cli --version 0.2.129（Cargo.toml の wasm-bindgen と同じ版）
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
cargo build --target wasm32-unknown-unknown --release --lib
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
wasm-bindgen target/wasm32-unknown-unknown/release/vstbridge.wasm --out-dir pkg --target nodejs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
wasm-bindgen target/wasm32-unknown-unknown/release/vstbridge.wasm --out-dir pkg-web --target web
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Host "pkg/（Node）と pkg-web/（ブラウザ）を作った"
