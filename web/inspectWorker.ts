// VST を Worker の中で読み込み、名前、識別番号、つまみの名前と値、重さの目安を調べる（inspect.ts）。読み込みは重い（大きい DLL で十数秒）ので、画面のスレッドから外す
/// <reference lib="webworker" />
import { initSync, JsVst } from './vstbridge.js'

export interface InspectRequest {
  wasm: ArrayBuffer
  dll: ArrayBuffer
}

/** 調べた結果。load は処理にかかった時間 ÷ 音の長さ（量れなければ undefined） */
export interface VstInspection {
  name: string
  uniqueId: number
  /** VST3 のクラスの id（VST2 は undefined） */
  cid?: string
  vst3: boolean
  params: { name: string; value: number }[]
  load?: number
}

export type InspectMessage = { ok: true; info: VstInspection } | { ok: false; error: string }

/** 重さを量る。雑音を 128 サンプルずつ通し、処理にかかった時間 ÷ 音の長さを返す（48 kHz とみなす。始めの数回は除く） */
function measureLoad(vst: JsVst): number {
  const n = 128
  const chans = Array.from({ length: Math.max(1, vst.numInputs) }, () => Float32Array.from({ length: n }, () => Math.random() * 0.2 - 0.1))
  for (let i = 0; i < 8; i++) vst.process(chans, n)
  const blocks = 96
  const t0 = performance.now()
  for (let i = 0; i < blocks; i++) vst.process(chans, n)
  return (performance.now() - t0) / 1000 / ((blocks * n) / 48000)
}

self.onmessage = (e: MessageEvent<InspectRequest>) => {
  try {
    initSync({ module: e.data.wasm })
    const vst = new JsVst(new Uint8Array(e.data.dll))
    const params = Array.from({ length: vst.numParams }, (_, i) => ({ name: vst.paramName(i), value: vst.getParam(i) }))
    let load: number | undefined
    try {
      load = measureLoad(vst)
    } catch {
      // 処理で落ちる VST は目安を出さない
    }
    const info: VstInspection = { name: vst.name(), uniqueId: vst.uniqueId(), cid: vst.classId() ?? undefined, vst3: vst.isVst3, params, load }
    vst.free()
    self.postMessage({ ok: true, info } satisfies InspectMessage)
  } catch (err) {
    self.postMessage({ ok: false, error: err instanceof Error ? err.message : String(err) } satisfies InspectMessage)
  }
}
