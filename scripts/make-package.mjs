// VST の DLL から、WeVocal プラグインのパッケージ（manifest と processor と wasm）を作る。
// 導入するときに 1 回だけ呼ぶ（つまみの数と名前は DLL ごとに違うので、DLL を一度読み込んで調べる）。ブラウザでも Node でも動く
// 使い方（Node）: node scripts/make-package.mjs <dll> <出力先フォルダー>
//   出力: manifest.json、processor.js、vstbridge_bg.wasm、plugin.dll（processor が読む DLL の写し。state ではなく wasm と同じ置き場に置く）
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { basename, join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { JsVst } from '../pkg/vstbridge.js'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')

/** AudioWorklet（TextDecoder も import.meta も無い）で動く形に、wasm-bindgen の web 版のグルーを加工する */
export function workletGlue() {
  let src = readFileSync(join(root, 'pkg-web/vstbridge.js'), 'utf8')
  // export を外す（クラス名と initSync はそのまま使う）
  src = src.replace(/^export class /m, 'class ').replace(/^export \{[^}]*\};?\s*$/m, '')
  // fetch を使う初期化（__wbg_init）は使わない。import.meta を含む行が構文エラーになるので消す
  src = src.replace(/async function __wbg_init[\s\S]*$/, '')
  // AudioWorkletGlobalScope には TextDecoder/TextEncoder が無いので、最小の UTF-8 の実装を先頭に足す
  const polyfill = `
if (typeof TextDecoder === 'undefined') {
  globalThis.TextDecoder = class {
    decode(b) {
      if (!b) return ''
      let s = ''
      for (let i = 0; i < b.length; ) {
        const c = b[i++]
        if (c < 0x80) s += String.fromCharCode(c)
        else if (c < 0xe0) s += String.fromCharCode(((c & 0x1f) << 6) | (b[i++] & 0x3f))
        else if (c < 0xf0) s += String.fromCharCode(((c & 0x0f) << 12) | ((b[i++] & 0x3f) << 6) | (b[i++] & 0x3f))
        else {
          const cp = ((c & 0x07) << 18) | ((b[i++] & 0x3f) << 12) | ((b[i++] & 0x3f) << 6) | (b[i++] & 0x3f)
          s += String.fromCodePoint(cp)
        }
      }
      return s
    }
  }
}
if (typeof TextEncoder === 'undefined') {
  globalThis.TextEncoder = class {
    encode(str) {
      const out = []
      for (const ch of str) {
        const c = ch.codePointAt(0)
        if (c < 0x80) out.push(c)
        else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f))
        else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f))
        else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 0x3f), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f))
      }
      const u = new Uint8Array(out)
      return u
    }
    encodeInto(str, dest) {
      const b = this.encode(str)
      const n = Math.min(b.length, dest.length)
      dest.set(b.subarray(0, n))
      return { read: str.length, written: n }
    }
  }
}
`
  return polyfill + src
}

/** id に使える形にする */
const slug = (s) => s.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '') || 'vst'

/** processor のソースを作る。つまみの一覧と DLL を埋め込まず、wasm と DLL は processorOptions で受け取る */
export function makeProcessor(id, params) {
  const descriptors = JSON.stringify(params.map((p) => ({ name: p.id, defaultValue: p.default, minValue: 0, maxValue: 1, automationRate: 'k-rate' })))
  const name = 'wevocal-fx-' + id.replace(/[^a-zA-Z0-9]/g, '-')
  return `${workletGlue()}
// ---- 以下が processor 本体 ----
class VstProcessor extends AudioWorkletProcessor {
  static get parameterDescriptors() { return ${descriptors} }
  constructor(options) {
    super()
    const o = options.processorOptions || {}
    this.ids = ${JSON.stringify(params.map((p) => p.id))}
    this.last = new Array(this.ids.length).fill(NaN)
    this.vst = null
    this.failed = false
    try {
      initSync({ module: o.wasm })
      this.vst = new JsVst(new Uint8Array(o.dll))
    } catch (e) {
      this.failed = true
      this.port.postMessage({ __wevocalError: String(e) })
    }
  }
  process(inputs, outputs, parameters) {
    const out = outputs[0]
    const inp = inputs[0]
    if (this.failed || !this.vst) {
      for (let c = 0; c < out.length; c++) if (inp[c]) out[c].set(inp[c])
      return true
    }
    // つまみが変わったときだけ VST に渡す
    for (let i = 0; i < this.ids.length; i++) {
      const v = parameters[this.ids[i]][0]
      if (v !== this.last[i]) { this.vst.setParam(i, v); this.last[i] = v }
    }
    const n = out[0].length
    const nin = this.vst.numInputs
    const chans = []
    for (let c = 0; c < nin; c++) chans.push(inp[c] ?? inp[0] ?? new Float32Array(n))
    const res = this.vst.process(chans, n)
    for (let c = 0; c < out.length; c++) out[c].set(res[c] ?? res[0])
    return true
  }
}
registerProcessor('${name}', VstProcessor)
`
}

/** DLL を読み込んでつまみを調べ、manifest を作る */
export function makeManifest(dll, fileName) {
  const v = new JsVst(dll)
  const name = v.name() || fileName.replace(/\.dll$/i, '')
  const hash = createHash('sha256').update(dll).digest('hex').slice(0, 12)
  const id = `vst.${slug(name)}-${hash}`
  const params = []
  const lang = { ja_jp: { name }, en_us: { name } }
  for (let i = 0; i < v.numParams; i++) {
    const pname = v.paramName(i)
    const pid = `p${i}`
    params.push({ id: pid, name: `param.${pid}`, min: 0, max: 1, default: v.getParam(i), step: 0.001 })
    lang.ja_jp[`param.${pid}`] = pname
    lang.en_us[`param.${pid}`] = pname
  }
  v.free()
  return {
    id,
    api: 1,
    kind: 'fx',
    format: 'vst',
    version: '1.0.0',
    author: 'VST',
    name: 'name',
    // 書き出しで掛ける（再生中はエミュレーターが間に合わないので素通し）
    realtime: false,
    anticipative: false,
    permissions: [],
    params,
    processor: 'processor.js',
    wasm: 'vstbridge_bg.wasm',
    lang,
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const [dllPath, outDir] = process.argv.slice(2)
  if (!dllPath || !outDir) {
    console.error('使い方: node scripts/make-package.mjs <dll> <出力先フォルダー>')
    process.exit(2)
  }
  const dll = readFileSync(dllPath)
  const manifest = makeManifest(dll, basename(dllPath))
  mkdirSync(outDir, { recursive: true })
  writeFileSync(join(outDir, 'manifest.json'), JSON.stringify(manifest, null, 2))
  writeFileSync(join(outDir, 'processor.js'), makeProcessor(manifest.id, manifest.params))
  writeFileSync(join(outDir, 'vstbridge_bg.wasm'), readFileSync(join(root, 'pkg-web/vstbridge_bg.wasm')))
  writeFileSync(join(outDir, 'plugin.dll'), dll)
  console.log(`${manifest.id}: ${manifest.params.length} 個のつまみ → ${outDir}`)
}
