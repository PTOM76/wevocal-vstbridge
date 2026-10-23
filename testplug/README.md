# 試すための VST2

エミュレーターを確かめるための、一番小さな VST2（音量を変えるだけ）と、ネイティブで動かすホスト。

- `vst2.h`: VST2 の形（構造体と関数の並び）を自分で書いたもの（SDK は使わない）
- `gain.c`: 音量を変えるだけの VST2。外部の DLL を import しない（-nostdlib）
- `sse.c`: SSE の型の変換（cvt）、並べ替え、movd、movq を使う VST2（JIT と、範囲外の変換の確かめ。tests/sse.rs、tests/jit.wasm.mjs）。ビルドは gain.c と同じで、名前だけ sse にする
- `intops.c`: 引数が 1 つの mul と imul、32 と 64 ビットの div と idiv、switch の表（間接の jmp）、x87 の fsin、fcos、fpatan を使う VST2（JIT の確かめ。tests/jit.wasm.mjs）。ビルドは gain.c と同じで、名前だけ intops にする
- `host.c`: ネイティブの Windows で gain.dll を動かすホスト（比べる基準）

## ビルド

DLL（エミュレーターと host.exe の両方で使う。64 ビット）:

```
x86_64-w64-mingw32-gcc -O2 -shared -nostdlib -fno-asynchronous-unwind-tables -Wl,--entry=DllMainCRTStartup -o gain.dll gain.c
```

ネイティブのホスト:

```
gcc -O2 -o host.exe host.c
./host.exe gain.dll
```

## 確かめ

`cargo test --test gain` が、gain.dll をエミュレーターで動かし、host.exe と同じ結果（名前 "WeVocal Test Gain"、音量 0.25 で 1 kHz の正弦波が 1/4 になる）になることを確かめる。gain.dll が無ければ飛ばす（上の命令でビルドする）。
