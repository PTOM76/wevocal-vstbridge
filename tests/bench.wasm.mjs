// wasm 化したエミュレーターの速さを測る（環境変数 SH1_REVERB_DLL か引数に DLL のパス）。先に ./build-wasm.sh
import { readFileSync } from 'node:fs'
import { JsVst } from '../pkg/vstbridge.js'

const path = process.argv[2] ?? process.env.SH1_REVERB_DLL
const secs = Number(process.argv[3] ?? 5)
const t0 = performance.now()
const v = new JsVst(readFileSync(path))
const t1 = performance.now()
const n = 128
const blocks = Math.floor((secs * 48000) / n)
for (let b = 0; b < blocks; b++) {
  const imp = new Float32Array(n)
  if (b === 0) imp[0] = 1
  v.process([imp, imp], n)
}
const t2 = performance.now()
console.log(`rss ${(process.memoryUsage().rss/1048576).toFixed(0)} MB`)
console.log(`読み込み ${(t1 - t0).toFixed(0)} ms、${secs} 秒分の処理 ${(t2 - t1).toFixed(0)} ms（実時間の ${((t2 - t1) / 1000 / secs).toFixed(2)} 倍）`)
