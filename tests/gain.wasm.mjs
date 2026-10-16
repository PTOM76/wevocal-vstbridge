// wasm 化した vstbridge（pkg/、build-wasm.sh で作る）を Node で動かし、ネイティブのホスト（host.exe）、
// Rust のテスト（tests/gain.rs）と同じ結果になるか確かめる。先に ./build-wasm.sh を実行すること
// 使い方: node tests/gain.wasm.mjs
import { readFileSync } from 'node:fs'
import { JsVst } from '../pkg/vstbridge.js'

const dll = readFileSync(new URL('../testplug/gain.dll', import.meta.url))
const v = new JsVst(dll)

const name = v.name()
if (name !== 'WeVocal Test Gain') throw new Error(`name: ${name}`)
if (v.numParams !== 1 || v.numInputs !== 2 || v.numOutputs !== 2) throw new Error('params/inputs/outputs')

v.setParam(0, 0.25)
if (Math.abs(v.getParam(0) - 0.25) > 1e-6) throw new Error('getParam')

const n = 8
const sine = Float32Array.from({ length: n }, (_, i) => Math.sin((2 * Math.PI * 1000 * i) / 48000))
const out = v.process([sine, sine], n)
// host.exe と Rust のテストと同じ出力（音量 0.25 を掛けたもの）
const want = [0, 0.032632, 0.064705, 0.095671, 0.125, 0.15219, 0.176777, 0.198338]
Array.from(out[0]).forEach((x, i) => {
  if (Math.abs(x - want[i]) > 1e-5) throw new Error(`sample ${i}: ${x} != ${want[i]}`)
})
if (out[0][0] !== out[1][0]) throw new Error('左右が同じでない')

console.log('ok')
