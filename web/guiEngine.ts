// VST の画面を動かす中身（DLL を読み込んで画面を開き、idle を回して描いた画を返す）。メインのスレッドでも Worker（guiWorker.ts）でも使う。閉じた画面はいくつか開いたまま残し、次に開くときに使い回す
import { JsVst } from './vstbridge.js'

/** 閉じた画面を開いたまま残しておく数。次に開くときに DLL の立ち上げ直しを省く */
const KEEP_MAX = 4
/** 閉じた画面を使わないまま残しておく時間（ミリ秒）。過ぎたら閉じてメモリを返す */
const KEEP_MS = 5 * 60 * 1000
/** 続けてこの回数失敗したら画面を止める */
const MAX_ERRORS = 5

export interface EditorEvents {
  /** 描き直した画（RGBA。変わったときだけ） */
  frame(px: Uint8Array): void
  /** 画面でつまみを変えた分 [index, 値, …] */
  changes(ch: Float32Array): void
  error(operation: string, error: unknown): void
  /** 続けて失敗して止まった */
  dead(): void
}

export interface EditorSession {
  size: [number, number]
  /** マウスの入力（msg は 0x200 移動、0x201 押した、0x202 離した。keys は bit0 左ボタン、bit1 Ctrl、bit2 Shift） */
  mouse(msg: number, x: number, y: number, keys: number): void
  /** ホストの値を画面のつまみへ渡す [index, 値] */
  setParams(list: [number, number][]): void
  /** 描き直す間隔（ミリ秒）。0 で止める */
  setRate(ms: number): void
  close(): void
}

type Editor = { key: string; vst: JsVst; size: [number, number]; timer?: ReturnType<typeof setTimeout> }
/** 閉じた画面（古い順） */
const idle: Editor[] = []
/** 試して画面を持たなかった DLL。次からは重い読み込みをしない */
const noEditor = new Set<string>()

function release(e: Editor) {
  clearTimeout(e.timer)
  try {
    e.vst.editorClose()
    e.vst.free()
  } catch (error) {
    console.error('Failed to close VST editor', { key: e.key, error })
  }
}

/** 残してある画面を使う。無ければ DLL を読み込んで開く。画面を持たない DLL は null */
function take(key: string, dll: ArrayBuffer): Editor | null {
  if (noEditor.has(key)) return null
  const i = idle.findIndex((e) => e.key === key)
  if (i >= 0) {
    const e = idle.splice(i, 1)[0]
    clearTimeout(e.timer)
    return e
  }
  const vst = new JsVst(new Uint8Array(dll))
  if (!vst.hasEditor()) {
    noEditor.add(key)
    vst.free()
    return null
  }
  const [w, h] = vst.editorOpen()
  return { key, vst, size: [w, h] }
}

/** 閉じた画面を残す。上限を超えたら古いものから閉じ、しばらく使わなければそれも閉じる */
function keep(e: Editor) {
  e.timer = setTimeout(() => {
    const i = idle.indexOf(e)
    if (i >= 0) release(idle.splice(i, 1)[0])
  }, KEEP_MS)
  idle.push(e)
  while (idle.length > KEEP_MAX) release(idle.shift()!)
}

/** 残してある画面をすべて閉じる */
export function releaseEditors() {
  idle.splice(0).forEach(release)
}

/** 画面を開く。key は DLL を見分ける名前（id と版と大きさ）。画面を持たない DLL は null。読み込みに失敗したら例外 */
export function openEditor(key: string, dll: ArrayBuffer, ev: EditorEvents): EditorSession | null {
  const kept = take(key, dll)
  if (!kept) return null
  const { vst } = kept
  // 前に開いていたときに溜まった変更は捨てる
  vst.editorChanges()

  let dead = false
  let closed = false
  let errors = 0
  let rate = 0
  let timer: ReturnType<typeof setTimeout> | undefined
  /** 前に描いた画面。変わっていなければ送らない */
  let prev: Uint32Array | null = null

  const safe = (operation: string, f: () => void) => {
    if (dead) return
    try {
      f()
      errors = 0
    } catch (error) {
      // 対応していない処理で 1 回失敗しても止めない。続けて失敗したときだけ止める（音は別のインスタンスなので影響しない）
      ev.error(operation, error)
      if (++errors >= MAX_ERRORS) {
        dead = true
        clearTimeout(timer)
        ev.dead()
      }
    }
  }
  const draw = () => {
    const px = vst.editorFrame()
    if (!px) return
    const now = new Uint32Array(px.buffer as ArrayBuffer, px.byteOffset, px.length >> 2)
    if (prev && prev.length === now.length) {
      let i = 0
      while (i < now.length && prev[i] === now[i]) i++
      if (i === now.length) return
    }
    // 送る側で手放す（Worker は transfer する）ので、比べる分は写しておく
    prev = now.slice()
    ev.frame(px)
  }
  const tick = () => {
    timer = undefined
    if (dead || closed || rate <= 0) return
    safe('editor idle and render', () => {
      vst.editorIdle()
      const ch = vst.editorChanges()
      if (ch.length) ev.changes(ch)
      draw()
    })
    if (!dead && !closed && rate > 0) timer = setTimeout(tick, rate)
  }

  return {
    size: kept.size,
    mouse(msg, x, y, keys) {
      safe(msg === 0x201 ? 'pointer down' : msg === 0x202 ? 'pointer up' : 'pointer move', () => vst.editorMouse(msg, x, y, keys))
    },
    setParams(list) {
      safe('host parameter update', () => list.forEach(([i, v]) => vst.setParam(i, v)))
    },
    setRate(ms) {
      const was = rate
      rate = ms
      // 止めていた、または間隔を詰めたときはすぐ回す
      if (ms > 0 && (was <= 0 || ms < was || !timer)) {
        clearTimeout(timer)
        timer = setTimeout(tick, 0)
      }
      if (ms <= 0) {
        clearTimeout(timer)
        timer = undefined
      }
    },
    close() {
      if (closed) return
      closed = true
      clearTimeout(timer)
      // 止まった画面は残さない
      if (dead) release(kept)
      else keep(kept)
    },
  }
}
