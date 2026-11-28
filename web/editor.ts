// VST 自身の画面（DLL が描く画面）を要素の中に出す。画面用のエミュレーター（guiEngine.ts）を、使えれば Worker、使えなければページのスレッドで動かし、
// 描いた画を canvas に映してマウスの操作を渡す。画面でつまみを動かしたら onChange で返し、ホストの値が変わったら update で画面のつまみへ渡す。画面を持たない DLL は null
import { openEditor, type EditorEvents, type EditorSession } from './guiEngine'
import { openRemoteEditor } from './guiRemote'

/** 画面を描き直す間隔（ミリ秒） */
const FRAME_MS = 33
/** 操作がないときの間隔（ミリ秒）。メーターなどの動きが見える程度に空ける */
const IDLE_MS = 100
/** 最後の操作からこの間は細かく描く（ミリ秒） */
const ACTIVE_MS = 1500

export interface VstEditorOptions {
  /** canvas を入れる要素 */
  el: HTMLElement
  /** エミュレーターの wasm（バイト列）と DLL */
  wasm: ArrayBuffer
  dll: ArrayBuffer
  /** DLL を見分ける名前（閉じた画面を使い回すときに使う。DLL が変われば変える） */
  key: string
  /** 今のつまみの値（番号の順。分からないものは undefined） */
  values: (number | undefined)[]
  /** 画面でつまみを動かした */
  onChange: (index: number, value: number) => void
  /** Worker で動かすか（既定は動かす） */
  worker?: boolean
}

export interface VstEditorHandle {
  size: [number, number]
  /** 表示の倍率 */
  setScale(scale: number): void
  /** ホストのつまみの値（番号の順。変わったものだけ画面へ渡す） */
  update(values: (number | undefined)[]): void
  dispose(): void
}

export async function mountVstEditor(o: VstEditorOptions): Promise<VstEditorHandle | null> {
  const { el, wasm, dll, key, onChange } = o
  const values = o.values
  const known: (number | undefined)[] = [...values]
  let dead = false
  let draw: (px: Uint8Array) => void = () => {}
  const ev: EditorEvents = {
    frame: (px) => draw(px),
    changes: (ch) => {
      for (let i = 0; i + 1 < ch.length; i += 2) {
        const k = ch[i]
        if (known[k] !== ch[i + 1]) {
          known[k] = ch[i + 1]
          onChange(k, ch[i + 1])
        }
      }
    },
    error: (operation, error) => console.error('VST editor error', { key, operation, error }),
    dead: () => {
      dead = true
    },
  }

  let session: EditorSession | null
  try {
    const remote = o.worker !== false ? openRemoteEditor(wasm, key, dll, ev) : undefined
    const r = remote ? await remote : 'failed'
    // Worker が使えない、または Worker の中で開けなかったときは、ページのスレッドで開く
    session = r === 'failed' ? openEditor(key, dll, ev) : r
  } catch (error) {
    console.error('Failed to open VST editor', { key, error })
    return null
  }
  if (!session) return null
  const s = session
  const [w, h] = s.size
  // 今のつまみの値を画面へ渡す
  s.setParams(values.flatMap((v, i) => (v !== undefined ? [[i, v] as [number, number]] : [])))

  const canvas = document.createElement('canvas')
  canvas.width = w
  canvas.height = h
  Object.assign(canvas.style, { display: 'block', margin: '0 auto', width: `${w}px`, height: `${h}px`, touchAction: 'none', userSelect: 'none' })
  const g = canvas.getContext('2d')!
  el.appendChild(canvas)
  draw = (px) => g.putImageData(new ImageData(new Uint8ClampedArray(px.buffer as ArrayBuffer, px.byteOffset, px.length), w, h), 0, 0)

  /** 最後に操作した時刻。操作の直後は細かく描き、そうでなければ間隔を空ける */
  let active = 0
  /** 画面のつまみを押している間 true */
  let pressed = false
  let rate = -1
  /** 描き直す間隔を決める。見えていない間（ダイアログの裏、別のタブ）は止める */
  const pace = () => {
    const ms = dead ? 0 : !canvas.isConnected || canvas.ownerDocument.hidden ? 0 : pressed || performance.now() - active < ACTIVE_MS ? FRAME_MS : IDLE_MS
    if (ms !== rate) s.setRate((rate = ms))
  }
  const pacer = setInterval(pace, IDLE_MS)
  pace()

  /** キャンバスの座標（画面の大きさに合わせて拡大縮小されても、DLL の座標に直す） */
  const pos = (e: PointerEvent): [number, number] => {
    const r = canvas.getBoundingClientRect()
    return [Math.round(((e.clientX - r.left) * w) / r.width), Math.round(((e.clientY - r.top) * h) / r.height)]
  }
  const keys = (e: PointerEvent, left: boolean) => (left ? 1 : 0) | (e.ctrlKey || e.metaKey ? 2 : 0) | (e.shiftKey ? 4 : 0)
  let last = ''
  const touch = () => {
    active = performance.now()
    pace()
  }
  const up = (e: PointerEvent) => {
    if (!pressed) return
    pressed = false
    const [x, y] = pos(e)
    s.mouse(0x202, x, y, keys(e, false))
  }
  canvas.onpointerdown = (e) => {
    if (e.button !== 0) return
    canvas.setPointerCapture(e.pointerId)
    pressed = true
    touch()
    const [x, y] = pos(e)
    last = `${x},${y}`
    s.mouse(0x201, x, y, keys(e, true))
  }
  canvas.onpointermove = (e) => {
    const [x, y] = pos(e)
    if (`${x},${y}` === last) return
    last = `${x},${y}`
    touch()
    s.mouse(0x200, x, y, keys(e, pressed))
  }
  canvas.onpointerup = up
  // 窓の外で離した、取り消された、キャプチャを失ったときも離したことにする（押したままの状態を残さない）
  canvas.onpointercancel = up
  canvas.onlostpointercapture = up

  return {
    size: [w, h],
    setScale(scale) {
      // 拡大してもドットがぼやけないようにする
      canvas.style.imageRendering = scale >= 1.5 ? 'pixelated' : 'auto'
      canvas.style.width = `${Math.round(w * scale)}px`
      canvas.style.height = `${Math.round(h * scale)}px`
    },
    update(v) {
      // ホスト側で変わったぶん（リセット、オートメーション、プリセット）だけ画面のつまみへ渡す
      const list: [number, number][] = []
      v.forEach((x, i) => {
        if (x !== undefined && x !== known[i]) {
          known[i] = x
          list.push([i, x])
        }
      })
      if (list.length) s.setParams(list)
    },
    dispose() {
      clearInterval(pacer)
      // 押したまま閉じたときは離したことにする（残した画面を次に開いたとき、ドラッグの途中のままにしない）
      if (pressed) {
        pressed = false
        s.mouse(0x202, 0, 0, 0)
      }
      canvas.remove()
      s.close()
    },
  }
}
