// VST を動かす AudioWorklet の processor のソースを作る。wasm（vstbridge）と DLL は processorOptions で受け取る
import { WORKLET_GLUE } from './workletGlue'


/**
 * processor のソース（つまみは k-rate の AudioParam。変わったときだけ VST に渡す）。wasm と DLL は processorOptions で受け取る。
 * 読み込み（DllMain、VSTPluginMain など）は重い（大きい DLL で数秒）ので、再生では JsVstLoader で 1 回の process ごとに約 1 ms ずつ進め、終わるまでは素通しにする
 * （音を出すスレッドを長く止めない）。書き出しや先に作る描画（offline）では、途中が素通しにならないよう最初の process で読み込みを済ませる。
 * どちらも constructor では読み込まない（Chrome は processor ができるまで画面のスレッドを待たせるので、画面が固まる）。
 * 読み込みの済んだ写し（processorOptions.snap、snapshot.ts）があれば、読み込まずに、別の wasm の instance を作ってメモリごと戻す（数十 ms）。
 * 外した processor の VST は置き場（poolKey ごと）に戻し、同じ AudioContext で次に作る processor が使い回す。name は registerProcessor の名前
 */
export function vstProcessorSource(name: string, poolKey: string, ids: string[], defaults: number[]): string {
  const descriptors = JSON.stringify(ids.map((name, i) => ({ name, defaultValue: defaults[i], minValue: 0, maxValue: 1, automationRate: 'k-rate' })))
  return `${WORKLET_GLUE}
class VstProcessor extends AudioWorkletProcessor {
  static get parameterDescriptors() { return ${descriptors} }
  constructor(options) {
    super()
    const o = options.processorOptions || {}
    this.ids = ${JSON.stringify(ids)}
    this.last = new Array(this.ids.length).fill(NaN)
    this.vst = null
    this.loader = null
    // 読み込みを 1 回に進める命令の数。20 回ぶんの合計の時間で、1 回が約 0.7 ms になるよう増減する
    // （AudioWorklet には performance が無く、Date は 1 ms 単位なので、1 回ずつは測れない）
    this.budget = 5000
    this.loadN = 0
    this.loadT0 = 0
    this.failed = false
    // 状態の塊（FX の項目の state。base64）。VST の準備ができたら戻す（ほかの DAW から読んだ、塊でしか状態を持たない VST のため）
    this.state = typeof o.state === 'string' ? o.state : ''
    this.chunk = decodeState(this.state)
    // 置き場は AudioContext の中で共有なので、プラグインごとに分ける
    const pools = (globalThis.__wevocalVstPool ||= {})
    const pool = (pools[${JSON.stringify(poolKey)}] ||= [])
    try {
      initSync({ module: o.wasm })
      this.vst = pool.pop() || null
      // 読み込みは process の中で進める（ここで済ませると、Chrome は processor ができるまで画面のスレッドを待たせる）
      this.snap = this.vst ? null : o.snap || null
      this.rs = null
      this.wasm = o.wasm
      // 写しを待つとき（再生中の VST3）は読み込まず、届いた写しから戻す（__wevocalSnap）
      if (!this.vst && !this.snap && !o.waitSnap) this.loader = new JsVstLoader(new Uint8Array(o.dll))
      this.dll = o.dll
      this.offline = !!o.offline
      this.waitSnap = !!o.waitSnap
    } catch (e) {
      this.failed = true
      this.port.postMessage({ __wevocalError: 'init: ' + String(e) })
    }
    // 外されたら、止まっていない VST を置き場に戻す（つまみは次の process で全部渡し直す）
    this.port.addEventListener('message', (e) => {
      // ホストで状態が変わった（元に戻すなど）。次の process で戻す
      if (e.data && typeof e.data.__wevocalSetState === 'string') {
        this.state = e.data.__wevocalSetState
        this.chunk = decodeState(this.state)
        return
      }
      // 待っていた写しが届いた。作れなかったとき（null）は DLL を読み込む
      if (e.data && '__wevocalSnap' in e.data) {
        if (this.vst || this.loader || this.failed) return
        if (e.data.__wevocalSnap) this.snap = e.data.__wevocalSnap
        else {
          try {
            this.loader = new JsVstLoader(new Uint8Array(this.dll))
          } catch (err) {
            this.failed = true
            this.port.postMessage({ __wevocalError: 'load: ' + String(err) })
          }
        }
        return
      }
      // オフからオンに戻されたとき、止まっていれば（素通しになっていれば）DLL を読み込み直して試す
      if (e.data && e.data.__wevocalRetry && this.failed && this.dll) {
        this.failed = false
        this.vst = null
        this.snap = null
        this.rs = null
        this.last.fill(NaN)
        this.chunk = decodeState(this.state)
        // 写しを待つ VST3 は、写しをもう一度もらって戻す（DLL を読み込むと音のスレッドを止める）
        if (this.waitSnap) {
          this.port.postMessage({ __wevocalWantSnap: true })
          return
        }
        try {
          this.loader = new JsVstLoader(new Uint8Array(this.dll))
        } catch (err) {
          this.failed = true
          this.port.postMessage({ __wevocalError: 'reload: ' + String(err) })
        }
        return
      }
      if (!e.data || !e.data.__wevocalDispose) return
      if (this.loader) {
        this.loader.free()
        this.loader = null
      }
      this.snap = null
      this.rs = null
      if (!this.vst) return
      if (!this.failed) pool.push(this.vst)
      this.vst = null
    })
    this.port.start()
  }
  process(inputs, outputs, parameters) {
    const out = outputs[0]
    const inp = inputs[0]
    if (this.snap && !this.failed) this.restore(this.offline)
    if (this.loader && !this.failed) this.stepLoad(this.offline)
    if (this.failed || !this.vst) {
      for (let c = 0; c < out.length; c++) if (inp[c]) out[c].set(inp[c])
      return true
    }
    // 状態の塊を戻す（つまみの値は、そのあとで全部渡し直す）
    if (this.chunk) {
      try {
        this.vst.setChunk(this.chunk)
      } catch (e) {
        this.port.postMessage({ __wevocalError: 'state: ' + String(e) })
      }
      this.chunk = null
      this.last.fill(NaN)
    }
    for (let i = 0; i < this.ids.length; i++) {
      const v = parameters[this.ids[i]][0]
      if (v !== this.last[i]) { this.vst.setParam(i, v); this.last[i] = v }
    }
    const n = out[0].length
    const chans = []
    for (let c = 0; c < this.vst.numInputs; c++) chans.push(inp[c] ?? inp[0] ?? new Float32Array(n))
    // エミュレーターが止まったら（対応していない命令など）、例外で processor ごと無音にならないよう、以後は素通しにする
    let res
    try {
      res = this.vst.process(chans, n)
    } catch (e) {
      this.failed = true
      this.port.postMessage({ __wevocalError: 'process: ' + String(e) })
      for (let c = 0; c < out.length; c++) if (inp[c]) out[c].set(inp[c])
      return true
    }
    // 出力に NaN や inf が混ざったら、そのブロックは 0 にして、以後は素通しにする（暴走したプラグインが音を壊し続けないように）
    let bad = false
    for (let c = 0; c < out.length && !bad; c++) {
      const r = res[c] ?? res[0]
      for (let i = 0; i < r.length; i++) if (!Number.isFinite(r[i])) { bad = true; break }
    }
    if (bad) {
      this.failed = true
      this.port.postMessage({ __wevocalError: 'non-finite output' })
      for (let c = 0; c < out.length; c++) out[c].fill(0)
      return true
    }
    for (let c = 0; c < out.length; c++) out[c].set(res[c] ?? res[0])
    return true
  }
  /**
   * 写しから戻す。戻せなければ、今までどおり DLL を読み込む。
   * 再生では 1 回の process で 32 ページ（2 MB）ずつ写す（一度に写すと音を出すスレッドが数十 ms 止まる）。書き出しや先に作る描画（all）は一度に写す
   */
  restore(all) {
    const s = this.snap
    try {
      if (!this.rs) {
        const b = __vstBridge()
        const w = b.initSync({ module: this.wasm })
        // 写しには 0 でないページだけがある。作ったばかりの instance で値のある所（最初の大きさの分）は 0 にしてから、ページを置く
        const fresh = w.memory.buffer.byteLength
        if (s.size > fresh) w.memory.grow(Math.ceil((s.size - fresh) / 65536))
        const mem = new Uint8Array(w.memory.buffer)
        mem.fill(0, 0, fresh)
        this.rs = { b, w, mem, data: new Uint8Array(s.data), i: 0 }
        if (!all) return
      }
      const r = this.rs
      const end = all ? s.pages.length : Math.min(s.pages.length, r.i + 32)
      for (; r.i < end; r.i++) r.mem.set(r.data.subarray(r.i * 65536, (r.i + 1) * 65536), s.pages[r.i] * 65536)
      if (r.i < s.pages.length) return
      const t = r.w.__wbindgen_externrefs
      if (t.length < s.refs) t.grow(s.refs - t.length)
      const v = r.b.JsVst.__wrap(s.ptr)
      v.afterRestore()
      this.vst = v
      this.snap = null
      this.rs = null
    } catch (e) {
      this.snap = null
      this.rs = null
      this.port.postMessage({ __wevocalError: 'restore: ' + String(e) })
      this.loader = new JsVstLoader(new Uint8Array(this.dll))
    }
  }
  /** 読み込みを少し進める。終わったら JsVst にする */
  stepLoad(all) {
    try {
      // 書き出しや先に作る描画（offline）は、途中が素通しにならないよう、最初の process で最後まで読み込む（描画のスレッドなので画面は止まらない）
      if (all) {
        while (!this.loader.step(1000000)) {}
        this.vst = this.loader.finish()
        this.loader = null
        return
      }
      if (this.loadN === 0) this.loadT0 = currentTimeMs()
      const done = this.loader.step(this.budget)
      if (++this.loadN >= 20) {
        const avg = (currentTimeMs() - this.loadT0) / this.loadN
        // 0 ms と測れたとき（速すぎる）は 2 倍まで。遅すぎれば割合で減らす
        this.budget = Math.round(Math.min(200000, Math.max(500, this.budget * Math.min(2, Math.max(0.25, 0.7 / Math.max(avg, 0.05))))))
        this.loadN = 0
      }
      if (done) {
        this.vst = this.loader.finish()
        this.loader = null
      }
    } catch (e) {
      this.failed = true
      this.loader = null
      this.port.postMessage({ __wevocalError: 'load: ' + String(e) })
    }
  }
}
// 状態（base64）をバイト列にする。空か読めなければ null（AudioWorklet には atob が無いので自前で読む）
function decodeState(s) {
  if (!s) return null
  const abc = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
  const out = []
  let bits = 0
  let n = 0
  for (const ch of s) {
    if (ch === '=') break
    const v = abc.indexOf(ch)
    if (v < 0) continue
    bits = (bits << 6) | v
    n += 6
    if (n >= 8) {
      n -= 8
      out.push((bits >> n) & 255)
    }
  }
  return out.length ? new Uint8Array(out) : null
}
// AudioWorklet には performance が無いことがあるので、無ければ Date で測る
function currentTimeMs() {
  return globalThis.performance ? globalThis.performance.now() : Date.now()
}
registerProcessor('${name}', VstProcessor)
`
}
