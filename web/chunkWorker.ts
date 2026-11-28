// VST に状態の塊（chunk）を戻してつまみの値を読む、またはつまみの値を渡して塊を読む（Worker）。ほかの DAW との間で、塊と Studio のつまみの値をそろえるため（DLL の読み込みは重いので、画面のスレッドから外す）
/// <reference lib="webworker" />
import { initSync, JsVst } from './vstbridge.js'

/** chunk を渡すとつまみの値を、params を渡すと塊を返す */
export interface ChunkRequest {
  wasm: ArrayBuffer
  dll: ArrayBuffer
  chunk?: Uint8Array
  params?: number[]
}

export type ChunkMessage = { ok: true; params: number[]; chunk?: Uint8Array } | { ok: false; error: string }

self.onmessage = (e: MessageEvent<ChunkRequest>) => {
  try {
    initSync({ module: e.data.wasm })
    const vst = new JsVst(new Uint8Array(e.data.dll))
    if (e.data.chunk) vst.setChunk(e.data.chunk)
    e.data.params?.forEach((v, i) => i < vst.numParams && vst.setParam(i, v))
    const chunk = e.data.params ? vst.getChunk() : undefined
    const params = Array.from({ length: vst.numParams }, (_, i) => vst.getParam(i))
    vst.free()
    self.postMessage({ ok: true, params, ...(chunk ? { chunk } : {}) } satisfies ChunkMessage)
  } catch (err) {
    self.postMessage({ ok: false, error: String(err) } satisfies ChunkMessage)
  }
}
