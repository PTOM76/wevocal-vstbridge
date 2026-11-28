//! USER32 と GDI32 の関数のうち、ワイド文字（〜W）、64 ビットの並び（WNDCLASSEX、CREATESTRUCT、PAINTSTRUCT）、DIB（CreateDIBSection、StretchDIBits）のもの。
//! JUCE などの新しいプラグインが使う。中身は gui_api.rs と同じ gui.rs、gdi.rs の上に作る
use crate::emu::{Emu, HostFn};
use crate::gdi::{Bitmap, Obj};
use crate::host_stubs::{arg, read_wstr};
use crate::mem::R;

fn a(e: &Emu, n: u64) -> R<u32> {
    Ok(arg(e, n)? as u32)
}
fn i(e: &Emu, n: u64) -> R<i32> {
    Ok(arg(e, n)? as u32 as i32)
}
/// ポインターの幅（バイト）
fn pw(e: &Emu) -> u64 {
    if e.bits() == 64 { 8 } else { 4 }
}
/// 偽のモニターのハンドル
const MONITOR: u64 = 0x0001_0001;

fn boxed(f: impl Fn(&mut Emu) -> R<u64> + 'static) -> Option<HostFn> {
    Some(Box::new(f))
}

/// BITMAPINFOHEADER から (幅, 高さ, 上の行から並ぶか, 1 画素のビット数)
fn bmi(e: &Emu, p: u64) -> R<(i32, i32, bool, u32)> {
    let w = e.mem.read_n(p + 4, 4)? as u32 as i32;
    let h = e.mem.read_n(p + 8, 4)? as u32 as i32;
    let bpp = e.mem.read_n(p + 14, 2)? as u32;
    Ok((w, h.abs(), h < 0, bpp))
}

/// 1 行のバイト数（4 バイトの境界にそろう）
fn stride(w: i32, bpp: u32) -> usize {
    (w as usize * bpp as usize / 8 + 3) & !3
}

/// ゲストのメモリの 24 か 32 ビットの画素（BGR、BGRA）を、上の行からの 0x00RRGGBB の並びにする
fn read_pixels(e: &Emu, bits: u64, w: i32, h: i32, top_down: bool, bpp: u32) -> R<Vec<u32>> {
    let (st, bytes) = (stride(w, bpp), bpp as usize / 8);
    let mut raw = vec![0u8; st * h as usize];
    e.mem.read(bits, &mut raw)?;
    let mut out = vec![0u32; (w * h) as usize];
    for y in 0..h {
        let row = if top_down { y } else { h - 1 - y } as usize;
        for x in 0..w as usize {
            let k = row * st + x * bytes;
            out[y as usize * w as usize + x] = u32::from_le_bytes([raw[k], raw[k + 1], raw[k + 2], 0]);
        }
    }
    Ok(out)
}

/// DIB のビットマップを、ゲストのメモリの今の画素で描き直す（プラグインが直接書いた分を反映する）
pub fn sync_dib(e: &mut Emu, bitmap: u32) -> R<()> {
    let Some(&(bits, w, h, td, bpp)) = e.gui.dibs.get(&bitmap) else { return Ok(()) };
    let px = read_pixels(e, bits, w, h, td, bpp)?;
    if let Some(Obj::Bitmap(b)) = e.gui.gdi.objs.get_mut(&bitmap) {
        b.px = px;
    }
    Ok(())
}

/// StretchDIBits、SetDIBitsToDevice の共通。bits と bmi の画素の (xs, ys) から w × h を dc の (xd, yd) へ写す（拡大縮小はしない）
fn put_dib(e: &mut Emu, dc: u32, (xd, yd, wd, hd): (i32, i32, i32, i32), (xs, ys): (i32, i32), bits: u64, info: u64) -> R<u64> {
    let (w, h, td, bpp) = bmi(e, info)?;
    if bpp != 24 && bpp != 32 {
        return Ok(0);
    }
    let all = read_pixels(e, bits, w, h, td, bpp)?;
    let px: Vec<u32> = (0..hd).flat_map(|j| (0..wd).map(move |k| (j, k))).map(|(j, k)| all.get(((ys + j) * w + xs + k) as usize).copied().unwrap_or(0)).collect();
    e.gui.gdi.put_pixels(dc, xd, yd, wd, hd, &px);
    Ok(hd as u64)
}

type Rect = (i32, i32, i32, i32);

fn read_rect(e: &Emu, p: u64) -> R<Rect> {
    let v = |k: u64| -> R<i32> { Ok(e.mem.read_n(p + k * 4, 4)? as u32 as i32) };
    Ok((v(0)?, v(1)?, v(2)?, v(3)?))
}

fn empty(r: Rect) -> bool {
    r.2 <= r.0 || r.3 <= r.1
}

fn region(e: &Emu, h: u32) -> Option<Rect> {
    match e.gui.gdi.objs.get(&h) {
        Some(Obj::Region(l, t, r, b)) => Some((*l, *t, *r, *b)),
        _ => None,
    }
}

/// 領域 h を長方形 r にする。戻り値は領域の種類（ERROR=0、NULLREGION=1、SIMPLEREGION=2）
fn set_region(e: &mut Emu, h: u32, r: Rect) -> u64 {
    match e.gui.gdi.objs.get_mut(&h) {
        Some(o @ Obj::Region(..)) => {
            *o = Obj::Region(r.0, r.1, r.2, r.3);
            if empty(r) { 1 } else { 2 }
        }
        _ => 0,
    }
}

pub fn resolve(name: &str) -> Option<HostFn> {
    match name {
        // ---- ウィンドウ ----
        "RegisterClassExW" | "RegisterClassW" | "RegisterClassExA" => {
            let (ex, wide) = (name.contains("Ex"), name.ends_with('W'));
            boxed(move |e| {
                let (wc, w) = (arg(e, 1)?, pw(e));
                // WNDCLASSEX は先頭に cbSize と style（8 バイト）、WNDCLASS は style だけ（ポインターの幅にそろう）
                let head = if ex { 8 } else { w };
                let wndproc = e.mem.read_n(wc + head, w as usize)? as u32;
                // そのあと cbClsExtra、cbWndExtra（8 バイト）、hInstance、hIcon、hCursor、hbrBackground、lpszMenuName、lpszClassName
                let after = head + w + 8;
                let bg = e.mem.read_n(wc + after + 3 * w, w as usize)? as u32;
                let cp = e.mem.read_n(wc + after + 5 * w, w as usize)?;
                let cname = if cp < 0x1_0000 { format!("#{cp}") } else if wide { read_wstr(e, cp)? } else { e.mem.read_cstr(cp, 128)? };
                let atom = 0xc000 + e.gui.classes.len() as u32;
                e.gui.classes.insert(cname, crate::gui::Class { atom, wndproc, bg });
                Ok(atom as u64)
            })
        }
        "UnregisterClassW" => boxed(|_| Ok(1)),
        "CreateWindowExW" => boxed(|e| {
            let cptr = arg(e, 2)?;
            let cname = if cptr < 0x1_0000 { e.gui.classes.iter().find(|(_, c)| c.atom == cptr as u32).map(|(n, _)| n.clone()).unwrap_or_default() } else { read_wstr(e, cptr)? };
            let (x, y, w, h) = (i(e, 5)?, i(e, 6)?, i(e, 7)?, i(e, 8)?);
            // CW_USEDEFAULT（0x80000000）は 0 にする
            let fix = |v: i32| if v == i32::MIN { 0 } else { v };
            let parent = a(e, 9)?;
            let hwnd = e.gui.add_window(&cname, parent, fix(x), fix(y), fix(w), fix(h), 0xd4d0c8);
            // CREATESTRUCT: lpCreateParams、hInstance、hMenu、hwndParent（ポインターの幅）、cy、cx、y、x、style（4 バイト）、名前、クラス（ポインター）、exStyle
            let p = pw(e);
            let cs = e.mem.alloc32(96) as u64;
            for (k, v) in [arg(e, 12)?, arg(e, 11)?, arg(e, 10)?, parent as u64].iter().enumerate() {
                e.mem.write_n(cs + k as u64 * p, p as usize, *v)?;
            }
            let o = 4 * p;
            for (k, v) in [h, w, y, x].iter().enumerate() {
                e.mem.write_n(cs + o + k as u64 * 4, 4, *v as u32 as u64)?;
            }
            e.mem.write_n(cs + o + 16, 4, arg(e, 4)?)?;
            let o2 = (o + 20).next_multiple_of(p);
            e.mem.write_n(cs + o2, p as usize, arg(e, 3)?)?;
            e.mem.write_n(cs + o2 + p, p as usize, cptr)?;
            e.mem.write_n(cs + o2 + 2 * p, 4, arg(e, 1)?)?;
            let has_proc = e.gui.classes.get(&cname).is_some_and(|c| c.wndproc != 0);
            if e.send_message(hwnd, 0x81, 0, cs as u32)? == 0 && has_proc {
                e.gui.remove_window(hwnd);
                return Ok(0);
            }
            if e.send_message(hwnd, 0x01, 0, cs as u32)? as u32 == u32::MAX {
                e.gui.remove_window(hwnd);
                return Ok(0);
            }
            Ok(hwnd as u64)
        }),
        // 作ったときは 1（WM_NCCREATE を続ける）。ほかは 0
        "DefWindowProcW" => boxed(|e| Ok(matches!(a(e, 2)?, 0x81 | 0x14) as u64)),
        "GetWindowLongPtrW" | "GetWindowLongW" | "GetWindowLongPtrA" => crate::gui_api::resolve("GetWindowLongA"),
        "SetWindowLongPtrW" | "SetWindowLongW" | "SetWindowLongPtrA" => crate::gui_api::resolve("SetWindowLongA"),
        "PeekMessageW" | "DispatchMessageW" => boxed(|_| Ok(0)),
        // あとで届ける（idle のとき。JUCE は非同期の処理をこれで進める）
        "PostMessageW" | "PostMessageA" => boxed(|e| {
            let m = (a(e, 1)?, a(e, 2)?, arg(e, 3)?, arg(e, 4)?);
            e.gui.posted.push(m);
            Ok(1)
        }),
"TranslateMessage" | "SetWindowTextW" | "TrackMouseEvent" | "SetFocus" | "SetCapture" | "ReleaseCapture" | "ValidateRect" => boxed(|_| Ok(1)),
        // 窓を作ったスレッド（どの窓もメインのスレッドで作る。id は GetCurrentThreadId のメインと同じ 1）と、プロセスの id（あれば書く）。
        // JUCE は今のスレッドと比べて、窓のスレッドかどうかを見る
        "GetWindowThreadProcessId" => boxed(|e| {
            let pid = arg(e, 2)?;
            if pid != 0 {
                e.mem.write_n(pid, 4, 1)?;
            }
            Ok(1)
        }),
        // 前面の窓と、マウスを捕まえている窓は、どちらもプラグインの画面の窓（ルート）とする
        "GetForegroundWindow" | "GetActiveWindow" | "GetFocus" => boxed(|e| Ok(e.gui.root as u64)),
        "LoadCursorW" | "LoadIconW" => boxed(|_| Ok(1)),
        // 描き直しは idle のたびに全部のウィンドウへ WM_PAINT を送る（paint_all）ので、ここでは何もしない
        "InvalidateRect" | "UpdateWindow" | "RedrawWindow" => boxed(|_| Ok(1)),
        "IsWindowVisible" | "IsWindow" => boxed(|e| Ok(e.gui.windows.contains_key(&a(e, 1)?) as u64)),
        "ShowWindow" => boxed(|_| Ok(0)),
        "GetClientRect" => boxed(|e| {
            let (h, p) = (a(e, 1)?, arg(e, 2)?);
            let Some(w) = e.gui.windows.get(&h) else { return Ok(0) };
            for (k, v) in [0, 0, w.w, w.h].iter().enumerate() {
                e.mem.write_n(p + k as u64 * 4, 4, *v as u32 as u64)?;
            }
            Ok(1)
        }),
        "SetWindowPos" => boxed(|e| {
            // SWP_NOSIZE=1、SWP_NOMOVE=2
            let (h, x, y, w, hh, fl) = (a(e, 1)?, i(e, 3)?, i(e, 4)?, i(e, 5)?, i(e, 6)?, a(e, 7)?);
            let Some(win) = e.gui.windows.get(&h) else { return Ok(0) };
            let (nw, nh) = if fl & 1 != 0 { (win.w, win.h) } else { (w, hh) };
            e.gui.resize(h, nw, nh, 0xd4d0c8);
            if fl & 2 == 0 {
                if let Some(win) = e.gui.windows.get_mut(&h) {
                    (win.x, win.y) = (x, y);
                }
            }
            Ok(1)
        }),
        // DPI の扱いは「モニターごと（V2）」と答える（DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 = -4、DPI_AWARENESS_PER_MONITOR_AWARE = 2）。画面は 96 DPI の 1 枚（GetDpiForWindow）
        "GetThreadDpiAwarenessContext" | "GetWindowDpiAwarenessContext" | "SetThreadDpiAwarenessContext" => boxed(|e| Ok(if e.bits() == 64 { (-4i64) as u64 } else { (-4i32) as u32 as u64 })),
        "GetAwarenessFromDpiAwarenessContext" => boxed(|_| Ok(2)),
        // EnumChildWindows(親, proc, lParam)。子（孫も）を作った順に proc(hwnd, lParam) へ渡し、0 が返ったらやめる
        "EnumChildWindows" => boxed(|e| {
            let (parent, f, lp) = (a(e, 1)?, arg(e, 2)?, arg(e, 3)?);
            let kids: Vec<u32> = e.gui.order.iter().copied().filter(|h| { let mut p = e.gui.windows.get(h).map(|w| w.parent).unwrap_or(0); while p != 0 && p != parent { p = e.gui.windows.get(&p).map(|w| w.parent).unwrap_or(0) } p == parent && *h != parent }).collect();
            use crate::emu::Arg;
            for h in kids {
                let args = [Arg::Int(h as u64), Arg::Int(lp)];
                let r = if e.bits() == 64 { e.call(f, &args)? } else { e.call32(f as u32, &args)? };
                if r & 0xffff_ffff == 0 {
                    break
                }
            }
            Ok(1)
        }),
        // フックは動かさないが、成功したことにする（0 だと失敗として扱うプラグインがある）
        "SetWindowsHookExW" | "SetWindowsHookExA" => boxed(|_| Ok(0x7f00)),
        "UnhookWindowsHookEx" | "ChangeWindowMessageFilterEx" | "RegisterTouchWindow" => boxed(|_| Ok(1)),
        // SetTimer(hwnd, id, 間隔 ms, TIMERPROC)。ウィンドウが無ければ番号を作って返す。同じウィンドウと番号なら置き換える
        "SetTimer" => boxed(|e| {
            let (h, id, every, proc) = (a(e, 1)?, arg(e, 2)?, arg(e, 3)? & 0xffff_ffff, arg(e, 4)?);
            let id = if h == 0 { 0x7000 + e.gui.timers.len() as u64 + 1 } else { id };
            let now = crate::win_stubs::clock_ms(e);
            e.gui.timers.retain(|t| !(t.0 == h && t.1 == id));
            e.gui.timers.push((h, id, every.max(10), proc, now + every.max(10)));
            Ok(id)
        }),
        "KillTimer" => boxed(|e| {
            let (h, id) = (a(e, 1)?, arg(e, 2)?);
            let n = e.gui.timers.len();
            e.gui.timers.retain(|t| !(t.0 == h && t.1 == id));
            Ok((e.gui.timers.len() < n) as u64)
        }),
        // ---- 描画 ----
        "BeginPaint" => boxed(|e| {
            let (h, ps) = (a(e, 1)?, arg(e, 2)?);
            let Some(w) = e.gui.windows.get(&h) else { return Ok(0) };
            let (surface, ww, wh) = (w.surface, w.w, w.h);
            let dc = e.gui.gdi.new_dc(surface);
            // PAINTSTRUCT: hdc（ポインターの幅）、fErase（4 バイト）、rcPaint（左、上、右、下）
            let p = pw(e);
            e.mem.write(ps, &[0u8; 72])?;
            e.mem.write_n(ps, p as usize, dc as u64)?;
            e.mem.write_n(ps + p + 12, 4, ww as u64)?;
            e.mem.write_n(ps + p + 16, 4, wh as u64)?;
            Ok(dc as u64)
        }),
        // 領域は長方形で持つ。戻り値は NULLREGION（1、空）か SIMPLEREGION（2）
        "CreateRectRgn" => boxed(|e| Ok(e.gui.gdi.add(Obj::Region(i(e, 1)?, i(e, 2)?, i(e, 3)?, i(e, 4)?)) as u64)),
        "CreateRectRgnIndirect" => boxed(|e| {
            let r = read_rect(e, arg(e, 1)?)?;
            Ok(e.gui.gdi.add(Obj::Region(r.0, r.1, r.2, r.3)) as u64)
        }),
        // 更新する範囲は、いつもウィンドウ全体（描き直しは全部のウィンドウへ送るため）
        "GetUpdateRgn" => boxed(|e| {
            let (h, rgn) = (a(e, 1)?, a(e, 2)?);
            let r = e.gui.windows.get(&h).map(|w| (0, 0, w.w, w.h)).unwrap_or_default();
            Ok(set_region(e, rgn, r))
        }),
        // CombineRgn(dst, a, b, mode)。RGN_AND=1、RGN_OR=2、RGN_XOR=3、RGN_DIFF=4、RGN_COPY=5（XOR と DIFF は a の長方形のまま）
        "CombineRgn" => boxed(|e| {
            let (dst, mode) = (a(e, 1)?, a(e, 4)?);
            let (Some(p), q) = (region(e, a(e, 2)?), region(e, a(e, 3)?)) else { return Ok(0) };
            let r = match (mode, q) {
                (1, Some(q)) => (p.0.max(q.0), p.1.max(q.1), p.2.min(q.2), p.3.min(q.3)),
                (2, Some(q)) if empty(p) => q,
                (2, Some(q)) if !empty(q) => (p.0.min(q.0), p.1.min(q.1), p.2.max(q.2), p.3.max(q.3)),
                _ => p,
            };
            Ok(set_region(e, dst, r))
        }),
        "GetRgnBox" => boxed(|e| {
            let (rgn, p) = (a(e, 1)?, arg(e, 2)?);
            let Some(r) = region(e, rgn) else { return Ok(0) };
            let r = if empty(r) { (0, 0, 0, 0) } else { r };
            for (k, v) in [r.0, r.1, r.2, r.3].iter().enumerate() {
                e.mem.write_n(p + k as u64 * 4, 4, *v as u32 as u64)?;
            }
            Ok(if empty(r) { 1 } else { 2 })
        }),
        "CreateDIBSection" => boxed(|e| {
            let (info, ppv) = (arg(e, 2)?, arg(e, 4)?);
            let (w, h, td, bpp) = bmi(e, info)?;
            if (bpp != 24 && bpp != 32) || w <= 0 || h <= 0 {
                return Ok(0);
            }
            let bits = e.mem.alloc32(stride(w, bpp) * h as usize) as u64;
            if ppv != 0 {
                e.mem.write_n(ppv, pw(e) as usize, bits)?;
            }
            let hb = e.gui.gdi.add(Obj::Bitmap(Bitmap::new(w, h, 0)));
            e.gui.dibs.insert(hb, (bits, w, h, td, bpp));
            Ok(hb as u64)
        }),
        // StretchDIBits(dc, xd, yd, wd, hd, xs, ys, ws, hs, bits, bmi, usage, rop)
        "StretchDIBits" => boxed(|e| {
            let (dst, src) = ((i(e, 2)?, i(e, 3)?, i(e, 4)?, i(e, 5)?), (i(e, 6)?, i(e, 7)?));
            put_dib(e, a(e, 1)?, dst, src, arg(e, 10)?, arg(e, 11)?)
        }),
        // SetDIBitsToDevice(dc, xd, yd, w, h, xs, ys, start, lines, bits, bmi, usage)
        "SetDIBitsToDevice" => boxed(|e| {
            let (dst, src) = ((i(e, 2)?, i(e, 3)?, i(e, 4)?, i(e, 5)?), (i(e, 6)?, i(e, 7)?));
            put_dib(e, a(e, 1)?, dst, src, arg(e, 10)?, arg(e, 11)?)
        }),
        // ---- 画面（モニター）と DPI。1920 × 1080、96 DPI の画面が 1 つあるものとする ----
        "GetDpiForWindow" | "GetDpiForSystem" => boxed(|_| Ok(96)),
        // HORZRES=8、VERTRES=10、BITSPIXEL=12、PLANES=14、LOGPIXELSX=88、LOGPIXELSY=90
        "GetDeviceCaps" => boxed(|e| {
            Ok(match a(e, 2)? {
                8 => 1920,
                10 => 1080,
                12 => 32,
                14 => 1,
                88 | 90 => 96,
                _ => 0,
            })
        }),
        "MonitorFromWindow" | "MonitorFromPoint" | "MonitorFromRect" => boxed(|_| Ok(MONITOR)),
        // EnumDisplayMonitors(dc, clip, proc, data)。proc(モニター, dc, 長方形, data) を 1 回呼ぶ
        "EnumDisplayMonitors" => boxed(|e| {
            let (f, data) = (arg(e, 3)?, arg(e, 4)?);
            let r = e.mem.alloc32(16) as u64;
            for (k, v) in [0u64, 0, 1920, 1080].iter().enumerate() {
                e.mem.write_n(r + k as u64 * 4, 4, *v)?;
            }
            use crate::emu::Arg;
            let args = [Arg::Int(MONITOR), Arg::Int(0), Arg::Int(r), Arg::Int(data)];
            if e.bits() == 64 { e.call(f, &args)? } else { e.call32(f as u32, &args)? };
            Ok(1)
        }),
        // MONITORINFO: cbSize、rcMonitor、rcWork、dwFlags（1 = 主の画面）
        "GetMonitorInfoW" | "GetMonitorInfoA" => boxed(|e| {
            let p = arg(e, 2)?;
            for (k, v) in [0u64, 0, 1920, 1080, 0, 0, 1920, 1040, 1].iter().enumerate() {
                e.mem.write_n(p + 4 + k as u64 * 4, 4, *v)?;
            }
            Ok(1)
        }),
        // WINDOWINFO: cbSize、rcWindow、rcClient（枠なし。同じ長方形）、…
        "GetWindowInfo" => boxed(|e| {
            let (h, p) = (a(e, 1)?, arg(e, 2)?);
            let Some(w) = e.gui.windows.get(&h) else { return Ok(0) };
            let (ww, wh) = (w.w, w.h);
            let (ox, oy) = e.gui.origin(h);
            for (k, v) in [ox, oy, ox + ww, oy + wh, ox, oy, ox + ww, oy + wh].iter().enumerate() {
                e.mem.write_n(p + 4 + k as u64 * 4, 4, *v as u32 as u64)?;
            }
            Ok(1)
        }),
        "SetMapMode" | "SetMapperFlags" | "SetStretchBltMode" | "SetGraphicsMode" | "GdiFlush" => boxed(|_| Ok(1)),
        _ => None,
    }
}
