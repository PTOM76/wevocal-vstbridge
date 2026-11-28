// VST の画面を Worker（guiWorker.ts）で開く、ページ側の窓口。Worker は 1 つにまとめ、画面ごとに番号で分ける。Worker が使えなければ null を返し、呼ぶ側はページのスレッドで開く
import type { EditorEvents, EditorSession } from './guiEngine'
import type { GuiEvent, GuiRequest } from './guiWorker'

/** 開いた結果。failed は Worker の中で開けなかった（ページのスレッドで開き直す） */
export type RemoteResult = EditorSession | null | 'failed'

type Pending = { ev: EditorEvents; resolve: (s: RemoteResult) => void }

let worker: Worker | null = null
/** Worker を作れなかった、または壊れた */
let broken = false
let next = 1
const pending = new Map<number, Pending>()
const live = new Map<number, EditorEvents>()

function send(m: GuiRequest, transfer: Transferable[] = []) {
  worker?.postMessage(m, transfer)
}

function onEvent(m: GuiEvent) {
  if (m.t === 'opened') {
    const p = pending.get(m.h)
    if (!p) return
    pending.delete(m.h)
    if (m.error) {
      console.warn('Failed to open VST editor in worker; retrying on the page thread', m.error)
      return p.resolve('failed')
    }
    if (!m.size) return p.resolve(null)
    live.set(m.h, p.ev)
    const h = m.h
    p.resolve({
      size: m.size,
      mouse: (msg, x, y, keys) => send({ t: 'mouse', h, msg, x, y, keys }),
      setParams: (list) => send({ t: 'params', h, list }),
      setRate: (ms) => send({ t: 'rate', h, ms }),
      close: () => {
        live.delete(h)
        send({ t: 'close', h })
      },
    })
    return
  }
  const ev = live.get(m.h)
  if (!ev) return
  if (m.t === 'frame') ev.frame(m.px)
  else if (m.t === 'changes') ev.changes(m.ch)
  else if (m.t === 'error') ev.error(m.operation, m.error)
  else if (m.t === 'dead') ev.dead()
}

function ensure(wasm: ArrayBuffer): Worker | null {
  if (worker || broken) return worker
  if (typeof Worker === 'undefined') {
    broken = true
    return null
  }
  try {
    worker = new Worker(new URL('./guiWorker.ts', import.meta.url), { type: 'module' })
  } catch (error) {
    console.warn('VST editor worker is not available; using the page thread', error)
    broken = true
    return null
  }
  worker.onmessage = (e: MessageEvent<GuiEvent>) => onEvent(e.data)
  worker.onerror = (e) => {
    // 読み込めない、または落ちた。待っている画面は null にし、開いている画面は止める
    console.error('VST editor worker failed', e.message)
    broken = true
    worker?.terminate()
    worker = null
    pending.forEach((p) => p.resolve('failed'))
    pending.clear()
    live.forEach((ev) => ev.dead())
    live.clear()
  }
  send({ t: 'init', wasm: wasm.slice(0) })
  return worker
}

/** Worker で画面を開く。Worker を使えないときは undefined（呼ぶ側がページのスレッドで開く）。画面を持たない DLL は null */
export function openRemoteEditor(wasm: ArrayBuffer, key: string, dll: ArrayBuffer, ev: EditorEvents): Promise<RemoteResult> | undefined {
  if (!ensure(wasm)) return undefined
  const h = next++
  return new Promise((resolve) => {
    pending.set(h, { ev, resolve })
    // 元のバイト列は音の側でも使うので、写して送る
    send({ t: 'open', h, key, dll: dll.slice(0) })
  })
}

/** Worker に残してある画面をすべて閉じる */
export function releaseRemoteEditors() {
  send({ t: 'releaseAll' })
}
