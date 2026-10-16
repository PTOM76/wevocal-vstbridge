# WeVocalVstBridge
WeVocalVstBridge は、Windows 向けの VST プラグイン（DLL）を、x86-64 のエミュレーターで動かすための研究用のライブラリである。最終的には wasm にして、iPad の PWA（Safari）の中で VST を直接動かすことを目指す。

WeVocal Studio と WeVocalSynth の試験的機能（WeVocal プラグインの「VST を読み込む」）のために作っている。**研究の段階**で、まだ実用にはならない。

## 目的
| 項目 | 内容 |
| --- | --- |
| 動かす場所 | PC に処理を任せず、iPad のブラウザの中で、利用者が指定した VST を動かす |
| 作り方 | Windows を丸ごとまねず、1 つの VST を動かすのに要る機能（PE の読み込み、要る API、VST の窓口、音の処理）に絞る |
| 速さ | 先読みで鳴らすリアルタイムを目指す。間に合わないものは書き出し専用に落とす |
| 本体とのつなぎ方 | WeVocal プラグイン（`format: "vst"`）の形だけ。本体の中には触れない |

設計と背景は、WeVocal Studio の `memo/effects.md`（6 章、7 章）にある。

## 今の状態
x64 の PE（DLL）を読み込み、命令を 1 つずつ実行して、VST2 の `processReplacing` を通すところまでを作っている途中である。

| ファイル | 中身 |
| --- | --- |
| `src/pe.rs` | PE の読み込み |
| `src/mem.rs` | エミュレーターのメモリ |
| `src/cpu.rs` | レジスタとフラグ |
| `src/exec.rs` | 整数の命令の実行（データの移動、四則演算、比較、分岐、呼び出し） |
| `src/sse.rs` | SSE の命令の実行 |
| `src/emu.rs` | エミュレーターの本体。Windows x64 の呼び出しの決まりで DLL の関数を呼び、ホストの関数（import、コールバック）は入口の番地で Rust に回す |
| `src/vst.rs` | VST2 の読み込み（`AEffect`、dispatcher、つまみ、`processReplacing`） |
| `testplug/` | 試すためのプラグインと、ネイティブのホスト |
| `tests/` | 確かめ（`gain.rs`） |

未対応の命令と API は、実行のログとエラーから洗い出して足していく。wasm 化、JIT、Windows の API の本格的な実装、VST3（64 ビット）への対応はまだである。

## 技術スタック
| 項目 | 内容 |
| --- | --- |
| 言語 | Rust（edition 2024） |
| 命令の解読 | [iced-x86](https://crates.io/crates/iced-x86) |
| PE の解析 | [object](https://crates.io/crates/object) |

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

## ドキュメント
| ドキュメント名 | リンク先 |
| --- | --- |
| 引き継ぎ（進め方と次にやること） | [HANDOFF.md](HANDOFF.md) |

## License
This project is licensed under the Mozilla Public License 2.0 (MPL-2.0).
