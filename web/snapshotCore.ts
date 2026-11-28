// VST を Worker で読み込み、wasm のメモリの写し（snapshot）を作る、または状態の塊とつまみの値を相互に読む。DLL の読み込みは重いので、画面と音のスレッドから外す
import type { SnapshotMessage, SnapshotRequest, VstSnapshot } from './snapshotWorker'
import type { ChunkMessage, ChunkRequest } from './chunkWorker'

export type { VstSnapshot }

/**
 * 写しの中身を SharedArrayBuffer に移す（使えるときだけ。crossOriginIsolated のページ）。
 * processor の作成（processorOptions）や Worker へ渡すたびに中身が複製されず、同じメモリを指すだけになる
 */
function share(s: VstSnapshot): VstSnapshot {
  if (typeof SharedArrayBuffer === 'undefined' || !globalThis.crossOriginIsolated) return s
  const data = new SharedArrayBuffer(s.data.byteLength)
  new Uint8Array(data).set(new Uint8Array(s.data))
  const pages = new Uint32Array(new SharedArrayBuffer(s.pages.byteLength))
  pages.set(s.pages)
  return { ...s, data, pages }
}

/** 写しを作る。作れなかったら null（呼ぶ側は DLL を読み込む）。wasm と dll は写して送るので、呼ぶ側の手元に残る。label は警告に添える名前 */
export function createVstSnapshot(wasm: ArrayBuffer, dll: ArrayBuffer, label = 'vst'): Promise<VstSnapshot | null> {
  return new Promise((resolve) => {
    if (typeof Worker === 'undefined') return resolve(null)
    const worker = new Worker(new URL('./snapshotWorker.ts', import.meta.url), { type: 'module' })
    const finish = (s: VstSnapshot | null) => {
      worker.terminate()
      resolve(s)
    }
    worker.onmessage = (e: MessageEvent<SnapshotMessage>) => {
      if (!e.data.ok) console.warn(`plugin ${label}: snapshot failed`, e.data.error)
      finish(e.data.ok ? share(e.data.snap) : null)
    }
    worker.onerror = (e) => {
      console.warn(`plugin ${label}: snapshot failed`, e.message)
      finish(null)
    }
    worker.postMessage({ wasm: wasm.slice(0), dll: dll.slice(0) } satisfies SnapshotRequest)
  })
}

/** Worker で DLL を読み込み、塊を戻すかつまみを渡したあとの結果を受け取る */
function askChunk(wasm: ArrayBuffer, dll: ArrayBuffer, req: Omit<ChunkRequest, 'wasm' | 'dll'>, label: string): Promise<Extract<ChunkMessage, { ok: true }> | null> {
  return new Promise((resolve) => {
    if (typeof Worker === 'undefined') return resolve(null)
    const worker = new Worker(new URL('./chunkWorker.ts', import.meta.url), { type: 'module' })
    const finish = (v: Extract<ChunkMessage, { ok: true }> | null) => {
      worker.terminate()
      resolve(v)
    }
    worker.onmessage = (e: MessageEvent<ChunkMessage>) => {
      if (!e.data.ok) console.warn(`plugin ${label}: chunk failed`, e.data.error)
      finish(e.data.ok ? e.data : null)
    }
    worker.onerror = () => finish(null)
    worker.postMessage({ wasm: wasm.slice(0), dll: dll.slice(0), ...req } satisfies ChunkRequest)
  })
}

/** 状態の塊を戻したときのつまみの値。読めなければ null */
export const paramsFromChunk = (wasm: ArrayBuffer, dll: ArrayBuffer, chunk: Uint8Array, label = 'vst'): Promise<number[] | null> => askChunk(wasm, dll, { chunk }, label).then((r) => r?.params ?? null)

/** つまみの値を渡したときの状態の塊（VST3 の状態、塊を使う VST2）。塊を使わない VST や読めないときは null */
export const chunkFromParams = (wasm: ArrayBuffer, dll: ArrayBuffer, params: number[], label = 'vst'): Promise<Uint8Array | null> => askChunk(wasm, dll, { params }, label).then((r) => r?.chunk ?? null)
