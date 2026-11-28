// VST の名前、識別番号、つまみ、重さの目安を、Worker で DLL を読み込んで調べる（画面のスレッドを止めない）。Worker が使えなければ、このスレッドで調べる
import type { InspectMessage, InspectRequest, VstInspection } from './inspectWorker'
import { initSync, JsVst } from './vstbridge.js'

export type { VstInspection }

/** 調べる。DLL でない、読み込めないときは Error（理由はエミュレーターのもの）。wasm と dll は写して送るので、呼ぶ側の手元に残る */
export function inspectVst(wasm: ArrayBuffer, dll: ArrayBuffer): Promise<VstInspection> {
  if (typeof Worker === 'undefined') return Promise.resolve(inspectHere(wasm, dll))
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL('./inspectWorker.ts', import.meta.url), { type: 'module' })
    worker.onmessage = (e: MessageEvent<InspectMessage>) => {
      worker.terminate()
      if (e.data.ok) resolve(e.data.info)
      else reject(new Error(e.data.error))
    }
    worker.onerror = (e) => {
      worker.terminate()
      reject(new Error(e.message))
    }
    worker.postMessage({ wasm: wasm.slice(0), dll: dll.slice(0) } satisfies InspectRequest)
  })
}

/** このスレッドで調べる（Worker の無い環境。重さは量らない） */
function inspectHere(wasm: ArrayBuffer, dll: ArrayBuffer): VstInspection {
  initSync({ module: wasm })
  const vst = new JsVst(new Uint8Array(dll))
  try {
    const params = Array.from({ length: vst.numParams }, (_, i) => ({ name: vst.paramName(i), value: vst.getParam(i) }))
    return { name: vst.name(), uniqueId: vst.uniqueId(), cid: vst.classId() ?? undefined, vst3: vst.isVst3, params }
  } finally {
    vst.free()
  }
}
