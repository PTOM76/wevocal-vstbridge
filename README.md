# WeVocalVstBridge
WeVocalVstBridge は、Windows 向けの VST プラグイン（DLL）を、x86-64 のエミュレーターで動かすためのライブラリである。最終的には wasm にして、PWA（ブラウザ）の中で VST を直接動かすことを目指す。

WeVocal Studio と WeVocalSynth で VST を読み込むための実行環境として作っている。開発中で、まだ実用にはならない。

## 特徴
| 項目 | 内容 |
| --- | --- |
| 動かす場所 | PC に処理を任せず、ブラウザの中で、利用者が指定した VST を動かす |
| 作り方 | Windows を丸ごとまねず、1 つの VST を動かすのに要る機能（PE の読み込み、要る API、VST の窓口、音の処理）に絞る |
| 速さ | 先読みで鳴らすリアルタイムを目指す。間に合わないものは書き出し専用に落とす |
| 本体とのつなぎ方 | WeVocal プラグイン（`format: "vst"`）の形だけ。本体の中には触れない |

## 今の状態
x64 の PE（DLL）を読み込み、命令を 1 つずつ実行して、VST2 の `processReplacing` を通せるようになった。試すための VST2（`testplug/gain.dll`、音量を変えるだけ）を、ネイティブのホストと、自前のエミュレーター（Rust）と、wasm にしたものの 3 通りで動かし、名前・つまみ・音の処理の結果がすべて一致することを確かめた。

| ファイル | 中身 |
| --- | --- |
| `src/pe.rs` | PE の読み込み |
| `src/mem.rs` | エミュレーターのメモリ |
| `src/cpu.rs` | レジスタとフラグ |
| `src/exec.rs` | 整数の命令の実行（データの移動、四則演算、比較、分岐、呼び出し） |
| `src/sse.rs` | SSE の命令の実行 |
| `src/emu.rs` | エミュレーターの本体。Windows x64 の呼び出しの決まりで DLL の関数を呼び、ホストの関数（import、コールバック）は入口の番地で Rust に回す |
| `src/vst.rs` | VST2 の読み込み（`AEffect`、dispatcher、つまみ、`processReplacing`） |
| `src/js.rs` | wasm-bindgen の窓口（`#[cfg(target_arch = "wasm32")]`）。JS から読み込み、つまみ、音の処理を使えるようにする |
| `testplug/` | 試すためのプラグインと、ネイティブのホスト |
| `tests/` | 確かめ（`gain.rs` は Rust のネイティブ、`gain.wasm.mjs` は Node で wasm を動かす確かめ） |

未対応の命令と API は、実行のログとエラーから洗い出して足していく。JIT、Windows の API の本格的な実装、VST3（64 ビット）への対応はまだである。ブラウザ（実機）で動かす確かめもまだ（Node では確かめた）。

## 技術スタック
| 項目 | 内容 |
| --- | --- |
| 言語 | Rust（edition 2024） |
| 命令の解読 | [iced-x86](https://crates.io/crates/iced-x86) |
| PE の解析 | [object](https://crates.io/crates/object) |
| wasm 化 | [wasm-bindgen](https://crates.io/crates/wasm-bindgen)（`wasm32-unknown-unknown`） |

## セットアップ
```bash
git clone git@github.com:PTOM76/wevocal-vstbridge.git
cd wevocal-vstbridge
cargo build
cargo test
```

`cargo test` には `testplug/gain.dll` が要る。`*.dll` と `*.exe` はリポジトリに含めないので、自分でビルドする（MinGW-w64 が要る）。

```bash
cd testplug
x86_64-w64-mingw32-gcc -O2 -shared -nostdlib -fno-asynchronous-unwind-tables -Wl,--entry=DllMainCRTStartup -o gain.dll gain.c
gcc -O2 -o host.exe host.c
./host.exe gain.dll
```

`host.exe` は、ネイティブの Windows で DLL を動かし、1 kHz の正弦波を通した結果と、名前、つまみの値を表示する。エミュレーターの結果は、これと比べる。

### wasm で確かめる

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129  # Cargo.toml の wasm-bindgen と同じ版
./build-wasm.sh    # pkg/（Node 向け）と pkg-web/（ブラウザ向け）を作る
node tests/gain.wasm.mjs
```

`pkg/` と `pkg-web/` は生成物（リポジトリに含めない）。

### wevocal-lib に入れる

```bash
./build-wasm.sh
node scripts/export-to-lib.mjs ../wevocalsynth/wevocal-lib/
```

wevocal-lib の `web/src/plugins/vst/` に `vstbridge.js`、`vstbridge.d.ts`、`vstbridge_bg.wasm`、`workletGlue.ts`（AudioWorklet 用に加工したグルー）を書き出す。同じフォルダーの `bridge.ts`、`gui.ts`、`index.ts` は lib 側で書くファイルなので上書きしない。書き出したあとは wevocal-lib でコミットし、Synth と Studio の submodule を更新する。

### ブラウザ（実機）で確かめる

```bash
cp -r pkg-web tests/browser/pkg-web
cp testplug/gain.dll tests/browser/gain.dll
node tests/browser/serve.mjs &
"/path/to/chrome" --headless=new --disable-gpu --no-sandbox --virtual-time-budget=5000 --dump-dom http://localhost:8991/
```

`tests/browser/index.html` が wasm を読み込み、`gain.dll` を動かした結果を画面（`<pre id="out">`）に出す。`RESULT: OK` ならネイティブと一致。`tests/browser/pkg-web` と `gain.dll` は生成物・バイナリなので含めない（上の手順でコピーする）。

## License
This project is licensed under the Mozilla Public License 2.0 (MPL-2.0).
