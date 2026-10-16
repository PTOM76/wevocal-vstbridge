# 試すための VST2

エミュレーターを確かめるための、一番小さな VST2（音量を変えるだけ）と、ネイティブで動かすホスト。

- `vst2.h`: VST2 の形（構造体と関数の並び）を自分で書いたもの（SDK は使わない）
- `gain.c`: 音量を変えるだけの VST2。外部の DLL を import しない（-nostdlib）
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
