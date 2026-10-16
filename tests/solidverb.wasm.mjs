// wasm 化した vstbridge で、本物の 32 ビット VST2 を動かす確かめ。リポジトリにはプラグインを含めないので、
// 環境変数 SOLIDVERB_DLL にパスを渡したときだけ実行する。先に ./build-wasm.sh を実行すること
import { readFileSync } from 'node:fs'
import { JsVst } from '../pkg/vstbridge.js'

const path = process.env.SOLIDVERB_DLL
if (!path) {
  console.log('skip: SOLIDVERB_DLL が無い')
  process.exit(0)
}
const v = new JsVst(readFileSync(path))
if (!v.is32bit) throw new Error('32 ビットのはず')
console.log('name:', v.name(), '/ params:', v.numParams, '/ in:', v.numInputs, '/ out:', v.numOutputs)
const names = Array.from({ length: v.numParams }, (_, i) => v.paramName(i))
console.log('param names:', names.join(', '))
if (names.length !== v.numParams) throw new Error('names')

const n = 64
const imp = new Float32Array(n)
imp[0] = 1
const out = v.process([imp, imp], n)
const energy = Array.from(out[0]).reduce((s, x) => s + Math.abs(x), 0)
console.log('energy:', energy.toFixed(4))
if (!(energy > 0) || !Array.from(out[0]).every(Number.isFinite)) throw new Error('出力がおかしい')
console.log('ok')
