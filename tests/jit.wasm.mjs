// JIT ありとなしで、同じ入力の出力がビット単位で一致するかと、速さを比べる。引数に DLL のパス（複数可）。先に ./build-wasm.sh
// 例: node tests/jit.wasm.mjs "…/SH-1 Reverb.dll" "…/SH-1 Delay.dll"
import { readFileSync } from 'node:fs'
import { JsVst } from '../pkg/vstbridge.js'

const paths = process.argv.slice(2)
if (!paths.length) { console.log('skip: DLL のパスが無い'); process.exit(0) }
const secs = Number(process.env.SECS ?? 2)  // MISSES=1 で、JIT で動かせなかった命令も表示する
const n = 128

/** 決まった雑音（同じ種から同じ列） */
function noise(seed) {
  let s = seed >>> 0
  return () => ((s = (s * 1664525 + 1013904223) >>> 0) / 2 ** 32) * 2 - 1
}

/** 2 秒分の雑音と、ときどきつまみを動かして流し、出力を全部つなげて返す */
function run(dll, jit) {
  const v = new JsVst(dll)
  v.setJit(jit)
  // DENY=bts,imul で、その命令を JIT で変換しない（不具合の切り分け）
  if (process.env.DENY) v.setJitDeny(process.env.DENY)
  if (jit && process.env.MISSES) v.countJitMisses()
  // THRESHOLD=32 で、変換を始める実行回数を変える
  if (process.env.THRESHOLD) v.setJitThreshold(Number(process.env.THRESHOLD))
  const ins = Math.max(1, v.numInputs), params = v.numParams
  // 入力が 0 の楽器には、ノートを送って音の処理を通す（50 ブロックごとに和音を変える）
  const synth = v.numInputs === 0
  let held = []
  const rnd = noise(1), knob = noise(2)
  const blocks = Math.floor((secs * 48000) / n)
  const out = []
  const t0 = performance.now()
  for (let b = 0; b < blocks; b++) {
    if (params && b % 50 === 0) v.setParam(Math.floor(((knob() + 1) / 2) * params) % params, (knob() + 1) / 2)
    if (synth && b % 50 === 0) {
      for (const k of held) v.sendMidi(0x80, k, 0, 0)
      held = [0, 1, 2].map(() => 48 + Math.floor(((knob() + 1) / 2) * 36))
      for (const k of held) v.sendMidi(0x90, k, 100, 0)
    }
    const input = Array.from({ length: ins }, () => Float32Array.from({ length: n }, () => rnd() * 0.5))
    for (const ch of v.process(input, n)) out.push(new Uint32Array(ch.buffer.slice(0)))
  }
  return { out, ms: performance.now() - t0, coverage: v.jitCoverage(), misses: jit ? v.jitMisses() : '', stats: v.jitStats().split(' ') }
}

let failed = 0
for (const path of paths) {
  const dll = readFileSync(path)
  const name = path.split(/[\\/]/).pop()
  try {
    const a = run(dll, false), b = run(dll, true)
    let diff = -1
    for (let k = 0; k < a.out.length && diff < 0; k++) for (let j = 0; j < a.out[k].length; j++) if (a.out[k][j] !== b.out[k][j]) { diff = k; break }
    if (diff >= 0) failed++
    console.log(`${diff < 0 ? 'ok  ' : 'NG  '}${name}: なし ${a.ms.toFixed(0)} ms、あり ${b.ms.toFixed(0)} ms（${(a.ms / b.ms).toFixed(2)} 倍）、JIT の命令 ${(b.coverage * 100).toFixed(0)}%、変換 ${b.stats[0]} 個 ${b.stats[1]} ms${diff >= 0 ? `、最初の違い ${diff} 番目の塊` : ''}`)
    // MISSES=1 なら、JIT で動かせなかったブロックの先頭の命令を多い順に表示する
    if (process.env.MISSES) console.log(b.misses.replace(/^/gm, '    '))
  } catch (e) {
    failed++
    console.log(`err ${name}: ${e.message ?? e}`)
  }
}
process.exit(failed ? 1 : 0)
