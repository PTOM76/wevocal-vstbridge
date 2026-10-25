// 読み込んだ VST を wasm のメモリごと写し、別の wasm（新しい instance）に戻して、元と同じ音が続くかを確かめる（Studio の先に作る描画で、DLL を毎回読み込まないため）。
// gain.dll は必ず、SH-1 Reverb（余韻があり、状態が続くか分かる）は環境変数 SH1_REVERB_DLL を渡したときだけ。先に ./build-wasm.sh
// 使い方: node tests/snapshot.wasm.mjs
import { readFileSync } from 'node:fs'

const wasmBytes = readFileSync(new URL('../pkg-web/vstbridge_bg.wasm', import.meta.url))
const module = new WebAssembly.Module(wasmBytes)
// 同じグルーを別の URL で読むと、別の instance になる
const fresh = async (tag) => {
  const g = await import(`../pkg-web/vstbridge.js?${tag}`)
  const wasm = g.initSync({ module })
  return { g, wasm }
}

/** 写す: メモリのバイト列、JsVst の番地、externref の表の長さ */
const snapshot = (wasm, vst) => ({ memory: wasm.memory.buffer.slice(0), ptr: vst.__wbg_ptr, refs: wasm.__wbindgen_externrefs.length })

/** 戻す: メモリを同じ大きさに広げて写し、表も広げ、JsVst にする */
function restore({ g, wasm }, s) {
  const mem = wasm.memory
  const need = s.memory.byteLength - mem.buffer.byteLength
  if (need > 0) mem.grow(need / 65536)
  new Uint8Array(mem.buffer).set(new Uint8Array(s.memory))
  const t = wasm.__wbindgen_externrefs
  if (t.length < s.refs) t.grow(s.refs - t.length)
  const v = g.JsVst.__wrap(s.ptr)
  v.afterRestore()
  return v
}

const n = 128
const signal = (k) => Float32Array.from({ length: n }, (_, i) => Math.sin((2 * Math.PI * 440 * (k * n + i)) / 48000) * (k < 4 ? 0.5 : 0))
const same = (a, b, what) => {
  for (let c = 0; c < a.length; c++)
    for (let i = 0; i < a[c].length; i++) if (Math.abs(a[c][i] - b[c][i]) > 1e-6) throw new Error(`${what}: ch${c} [${i}] ${a[c][i]} != ${b[c][i]}`)
}

async function check(label, dll, params) {
  const a = await fresh(`${label}-a`)
  const va = new a.g.JsVst(dll)
  params.forEach((x, i) => va.setParam(i, x))
  // 少し鳴らしてから写す（状態が続くか）
  for (let k = 0; k < 6; k++) va.process([signal(k), signal(k)], n)
  const t0 = performance.now()
  const s = snapshot(a.wasm, va)
  const b = await fresh(`${label}-b`)
  const vb = restore(b, s)
  const ms = performance.now() - t0
  for (let k = 6; k < 40; k++) {
    const x = signal(k)
    same(va.process([x, x], n), vb.process([x, x], n), `${label} block ${k}`)
  }
  console.log(`${label}: ok（メモリ ${(s.memory.byteLength / 1048576).toFixed(1)} MB、写して戻すのに ${ms.toFixed(0)} ms）`)
}

await check('gain', readFileSync(new URL('../testplug/gain.dll', import.meta.url)), [0.25])
if (process.env.SH1_REVERB_DLL) await check('sh1-reverb', readFileSync(process.env.SH1_REVERB_DLL), [])
else console.log('skip: SH1_REVERB_DLL が無い')
