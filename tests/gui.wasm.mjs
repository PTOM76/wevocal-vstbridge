// wasm 化したエミュレーターで、SH-1 Reverb の画面を開いて、つまみをドラッグできるかを確かめる（環境変数 SH1_REVERB_DLL にパスを渡したときだけ）。先に ./build-wasm.sh
import { readFileSync } from 'node:fs'
import { JsVst } from '../pkg/vstbridge.js'

const path = process.env.SH1_REVERB_DLL
if (!path) { console.log('skip: SH1_REVERB_DLL が無い'); process.exit(0) }
const v = new JsVst(readFileSync(path))
if (!v.hasEditor()) throw new Error('画面が無い')
const [w, h] = v.editorOpen()
if (w !== 421 || h !== 170) throw new Error(`大きさ ${w}x${h}`)
const px = v.editorFrame()
if (px.length !== w * h * 4) throw new Error('絵の大きさ')
// 背景だけ（単色）ではないこと
const first = px.slice(0, 3).join()
let different = 0
for (let i = 0; i < px.length; i += 4) if (px.slice(i, i + 3).join() !== first) different++
if (different < 1000) throw new Error('絵が空')
// TIME のつまみを上へドラッグ
v.editorMouse(0x201, 73, 50, 1); v.editorIdle()
for (const y of [40, 25, 10]) { v.editorMouse(0x200, 73, y, 1); v.editorIdle() }
const ch1 = Array.from(v.editorChanges())
v.editorMouse(0x202, 73, 10, 0); v.editorIdle()
if (ch1.length < 2 || ch1[0] !== 0 || !(ch1.at(-1) > 0.5)) throw new Error('つまみが動かない ' + ch1)
console.log(`ok（${w}x${h}、絵素 ${different}、変更 ${ch1.length / 2} 回、最後 ${ch1.at(-1).toFixed(3)}）`)
