// wasm 化したエミュレーターで、SH-1 Reverb が本物の CPU の出力と一致するかを確かめる（環境変数 SH1_REVERB_DLL にパスを渡したときだけ）。先に ./build-wasm.sh
import { readFileSync } from 'node:fs'
import { JsVst } from '../pkg/vstbridge.js'

const path = process.env.SH1_REVERB_DLL
if (!path) { console.log('skip: SH1_REVERB_DLL が無い'); process.exit(0) }
const v = new JsVst(readFileSync(path))
const n = 128
const energies = []
for (let b = 0; b < 200; b++) {
  const imp = new Float32Array(n)
  if (b === 0) imp[0] = 1
  const out = v.process([imp, imp], n)
  if (!Array.from(out[0]).every(Number.isFinite)) throw new Error(`ブロック ${b} で NaN`)
  energies.push(Array.from(out[0]).reduce((s, x) => s + Math.abs(x), 0))
}
// 本物の CPU での値（testplug/native32）
const want = [1.0, 0, 0, 0, 0, 0, 0, 0, 0, 0.14, 0.118, 0.129]
want.forEach((w, i) => { if (Math.abs(energies[i] - w) > 0.002) throw new Error(`ブロック ${i}: ${energies[i]} != ${w}`) })
const total = energies.reduce((a, b) => a + b, 0)
if (Math.abs(total - 17.1494) > 0.01) throw new Error(`合計 ${total}`)
console.log(`ok（合計 ${total.toFixed(4)}、最後のブロック ${energies.at(-1).toFixed(4)}）`)
