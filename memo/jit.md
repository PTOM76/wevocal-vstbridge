# JIT の設計

wasm の中で動くエミュレーターを速くするため、よく通る x86 のコードを、その場で wasm の関数に変換して動かす。ネイティブ（cargo test、examples）は今のインタープリターのままにする。

## 目標
- KarmaFX Reverb（今は wasm で実時間の約 15 倍）、Acon Digital Multiply（約 50 倍）を、実時間に近づける
- JIT ありとなしで、出力がビット単位で一致すること

## 段階 0: ゲストのメモリを 4GB 未満にそろえる
- 64 ビットの DLL を、PE の再配置（IMAGE_REL_BASED_DIR64）で 4GB 未満の番地に置き直す。スタック、ホストの確保（alloc）、TLS も 4GB 未満にする
- これで 32 ビットと 64 ビットのどちらでも、ゲストの番地 → ホストの番地を、ページ表（Memory::pages。2^20 個の u32）で引ける
- インタープリターの読み書きの近道も 64 ビットに効く（それだけで速くなる）

## 段階 1: ブロック単位の JIT
### 呼び出し方
- 生成した wasm は、Rust の wasm と同じ memory と関数の表（wasm_bindgen::memory()、wasm_bindgen::function_table()）を import する
- js_sys で `WebAssembly.Module` と `Instance` を作り、出来た関数を表に足す（table.grow、table.set）。Rust からは、その添字を `extern "C" fn(u32) -> u32` に変えて直接呼ぶ（JS を通らない）
- 生成した関数の引数は Cpu の番地。Cpu は `#[repr(C)]` にして、レジスタ、フラグ、xmm の位置を `offset_of!` で求めて埋め込む
- 戻り値は実行した命令の数。rip は生成したコードが Cpu に書く

### メモリの読み書き
- 読み: `p = pages[addr >> 12]`。p が 0、またはページの終わりをまたぐときは、遅い道（Rust の関数。表の添字で call_indirect）を呼ぶ
- 書き: 上に加えて、`code_pages[addr >> 12]` が 0 でない（命令として読んだページ）ときは遅い道へ。遅い道が、デコード済みの命令を捨てる合図（code_dirty）を立てる
- 遅い道で Fault が出たら、Emu に理由を置き、生成したコードはその命令の番地を rip にして戻る。Rust 側が Fault として返す

### 変換の単位
- blocks.rs と同じ基本ブロック。同じブロックが 32 回実行されたら変換する
- 変換できない命令に来たら、その手前でブロックを終える（残りはインタープリター）
- ブロックの最後の分岐が自分の頭へ戻るときは、wasm の loop にする（内側のループが wasm の中で回る）。命令数の上限の見張りのため、回数を数えて一定数で抜ける
- コードの書き換え（code_dirty）があったら、変換したものも捨てる（表の場所は使い回す）

### フラグ
- フラグは Cpu の Flags（cf、zf、sf、of、pf の bool）に置く
- ブロックの中で後ろから調べ、読まれる前に上書きされるフラグは作らない。ブロックの終わりではすべて生きているものとして扱う
- jcc、setcc、cmovcc は、直前の cmp、test、sub の引数から条件を直接求める（フラグを読み直さない）

### 段階 1 で変換する命令
- 整数: mov、movzx、movsx、movsxd、lea、add、sub、cmp、and、or、xor、test、inc、dec、neg、not、shl、shr、sar（即値、cl）、imul（2 つ、3 つの引数）、bt、bts、btr、cmovcc、setcc、jmp、jcc
- SSE: movss、movsd、movaps、movups、movapd、movupd、movdqa、movdqu、movd、movq、addss/sd、subss/sd、mulss/sd、divss/sd、sqrtss/sd、minss/sd、maxss/sd、addps/pd、subps/pd、mulps/pd、divps、andps/pd、orps、xorps/pd、andnps、pxor、pand、por、cvtsi2ss/sd、cvttss2si、cvttsd2si、cvtss2sd、cvtsd2ss、comiss、ucomiss、comisd、ucomisd、shufps、unpcklps
- packed の計算は wasm の SIMD（v128）を使う
- x86 と wasm で結果が違うところは、x86 に合わせる: minss/maxss の NaN と等しいとき（a < b ? a : b）、cvtt の範囲外と NaN（0x80000000）
- call、ret、push、pop、x87 は段階 1 では変換しない（インタープリター）

## 段階 2 以降
- call と ret、push と pop
- x87（SH-1 などの 32 ビットの古いプラグイン）
- 複数のブロックをまとめた関数（分岐の先も同じ関数の中で回す）
- ブロックをまたいだフラグの省略

## 確かめ方
- `JsVst::set_jit(bool)` で切り替え、Node で同じ入力を流して出力を比べる（tests/jit.wasm.mjs）。SH-1 の 7 つ、KarmaFX の 4 つ、Multiply で、2 秒分の雑音とつまみの変化を通し、ビット単位で一致すること
- 速さは、同じテストの時間で比べる

## 決めたこと
- 命令の解読は iced をそのまま使う。wasm のバイト列は wasm-encoder で作る
- AudioWorklet の中でも、同期の `new WebAssembly.Module` で作る（ブロックは小さい）
