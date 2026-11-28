// VST を読み込んで wasm のメモリの写しを作る（Worker）
/// <reference lib="webworker" />
// VST を Worker の中で読み込み、wasm のメモリごと写して返す（snapshot.ts）。読み込みは重い（大きい DLL で十数秒）ので、画面と音のスレッドから外す
import { initSync, JsVst } from './vstbridge.js'

export interface SnapshotRequest {
  wasm: ArrayBuffer
  dll: ArrayBuffer
}

/** 写し。読み込みが済んだときの wasm のメモリのうち、0 でないページ（64 KiB）だけを持つ。size はメモリの大きさ、ptr は JsVst の番地、refs は externref の表の長さ */
export interface VstSnapshot {
  size: number
  pages: Uint32Array
  /** ページでは、使えるときは SharedArrayBuffer（snapshot.ts の share） */
  data: ArrayBuffer | SharedArrayBuffer
  ptr: number
  refs: number
}

const PAGE = 65536

/** 0 でないページだけを詰める（VST の確保したメモリは 0 のままの所が多い。SH-1 Reverb で 22 MB → 4 MB） */
function pack(buf: ArrayBuffer): { size: number; pages: Uint32Array; data: ArrayBuffer } {
  const u = new Uint8Array(buf)
  const words = new Uint32Array(buf)
  const keep: number[] = []
  for (let p = 0; p < u.length / PAGE; p++) {
    const w0 = (p * PAGE) / 4
    for (let i = w0; i < w0 + PAGE / 4; i++)
      if (words[i]) {
        keep.push(p)
        break
      }
  }
  const data = new Uint8Array(keep.length * PAGE)
  keep.forEach((p, i) => data.set(u.subarray(p * PAGE, (p + 1) * PAGE), i * PAGE))
  return { size: u.length, pages: Uint32Array.from(keep), data: data.buffer }
}

export type SnapshotMessage = { ok: true; snap: VstSnapshot } | { ok: false; error: string }

self.onmessage = (e: MessageEvent<SnapshotRequest>) => {
  try {
    const wasm = initSync({ module: e.data.wasm })
    const vst = new JsVst(new Uint8Array(e.data.dll))
    // JS から何も呼んでいない間に写す（スタックは空で、メモリの中だけで状態が閉じている）
    const snap: VstSnapshot = { ...pack(wasm.memory.buffer), ptr: (vst as unknown as { __wbg_ptr: number }).__wbg_ptr, refs: wasm.__wbindgen_externrefs.length }
    self.postMessage({ ok: true, snap } satisfies SnapshotMessage, [snap.data, snap.pages.buffer])
  } catch (err) {
    self.postMessage({ ok: false, error: String(err) } satisfies SnapshotMessage)
  }
}
