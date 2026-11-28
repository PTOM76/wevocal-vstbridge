// VST の画面を Worker で動かす（guiEngine.ts をメッセージで操作する）。DLL の読み込みと画面の処理で、ページのスレッドを止めない
/// <reference lib="webworker" />
import { openEditor, releaseEditors, type EditorSession } from './guiEngine'
import { initSync } from './vstbridge.js'

/** ページから Worker へ。h は開いた画面の番号 */
export type GuiRequest =
  | { t: 'init'; wasm: ArrayBuffer }
  | { t: 'open'; h: number; key: string; dll: ArrayBuffer }
  | { t: 'mouse'; h: number; msg: number; x: number; y: number; keys: number }
  | { t: 'params'; h: number; list: [number, number][] }
  | { t: 'rate'; h: number; ms: number }
  | { t: 'close'; h: number }
  | { t: 'releaseAll' }

/** Worker からページへ。size が null なら画面を持たない DLL */
export type GuiEvent =
  | { t: 'opened'; h: number; size: [number, number] | null; error?: string }
  | { t: 'frame'; h: number; px: Uint8Array }
  | { t: 'changes'; h: number; ch: Float32Array }
  | { t: 'error'; h: number; operation: string; error: string }
  | { t: 'dead'; h: number }

const sessions = new Map<number, EditorSession>()
const post = (m: GuiEvent, transfer: Transferable[] = []) => self.postMessage(m, transfer)

self.onmessage = (e: MessageEvent<GuiRequest>) => {
  const m = e.data
  if (m.t === 'init') {
    initSync({ module: m.wasm })
    return
  }
  if (m.t === 'releaseAll') return releaseEditors()
  if (m.t === 'open') {
    const h = m.h
    try {
      const s = openEditor(m.key, m.dll, {
        frame: (px) => post({ t: 'frame', h, px }, [px.buffer]),
        changes: (ch) => post({ t: 'changes', h, ch }, [ch.buffer]),
        error: (operation, error) => post({ t: 'error', h, operation, error: String(error) }),
        dead: () => post({ t: 'dead', h }),
      })
      if (s) sessions.set(h, s)
      post({ t: 'opened', h, size: s ? s.size : null })
    } catch (error) {
      post({ t: 'opened', h, size: null, error: String(error) })
    }
    return
  }
  const s = sessions.get(m.h)
  if (!s) return
  if (m.t === 'mouse') s.mouse(m.msg, m.x, m.y, m.keys)
  else if (m.t === 'params') s.setParams(m.list)
  else if (m.t === 'rate') s.setRate(m.ms)
  else if (m.t === 'close') {
    s.close()
    sessions.delete(m.h)
  }
}
