//! USER32 と GDI32 の関数の本体。プラグインのエディターが画面を作って描くのに要るものを、gui.rs と gdi.rs の上に実装する。
//! 引数はスタックから読む（host_stubs.rs の arg）。ハンドルは gdi.rs の番号、座標は符号つき 32 ビット
use crate::emu::{Emu, HostFn};
use crate::gdi::{swap_rb, Bitmap, Obj, STOCK_BASE};
use crate::host_stubs::arg;
use crate::mem::R;
use crate::rsrc::{self, ResName};

fn a(e: &Emu, n: u64) -> R<u32> {
    Ok(arg(e, n)? as u32)
}
fn i(e: &Emu, n: u64) -> R<i32> {
    Ok(arg(e, n)? as u32 as i32)
}
fn rect(e: &Emu, p: u32) -> R<(i32, i32, i32, i32)> {
    let r = |o: u64| e.mem.read_n(p as u64 + o, 4).map(|v| v as u32 as i32);
    Ok((r(0)?, r(4)?, r(8)?, r(12)?))
}

/// システムカラー（COLOR_xxx）の色。Windows XP の既定の配色に近いもの
fn sys_color(idx: u32) -> u32 {
    match idx {
        5 => 0xffffff,
        8 | 18 => 0,
        16 => 0x808080,
        20 => 0xffffff,
        _ => 0xd4d0c8,
    }
}

/// ブラシの引数（ブラシのハンドルか、COLOR_xxx + 1）の色
fn brush_color(e: &Emu, h: u32) -> Option<u32> {
    if h < 32 {
        Some(sys_color(h.saturating_sub(1)))
    } else {
        e.gui.gdi.brush_rgb(h)
    }
}

/// 名前の引数（文字列へのポインター、または MAKEINTRESOURCE の数字）
fn res_name(e: &Emu, p: u32) -> R<ResName> {
    Ok(if p < 0x1_0000 { ResName::Id(p) } else { ResName::Name(e.mem.read_cstr(p as u64, 128)?) })
}

fn boxed(f: impl Fn(&mut Emu) -> R<u64> + 'static) -> Option<HostFn> {
    Some(Box::new(f))
}

/// 画面の API。VSTBRIDGE_GUI=1 のときは、呼び出しと戻り値を表示する（調査用）
pub fn resolve(name: &str) -> Option<HostFn> {
    let f = resolve_inner(name)?;
    if std::env::var("VSTBRIDGE_GUI").is_err() {
        return Some(f);
    }
    let n = name.to_string();
    Some(Box::new(move |e: &mut Emu| {
        let args: Vec<String> = (1..=crate::host_stubs::arg_count(&n).max(1)).map(|k| arg(e, k).map(|v| format!("{:x}", v as u32)).unwrap_or_default()).collect();
        let r = f(e);
        let ret = e.mem.read_n(e.cpu.regs[crate::cpu::RSP] - 4, 4).unwrap_or(0);
        eprintln!("[gui] {n}({}) -> {:x?} (呼び出し元 {ret:x})", args.join(", "), r.as_ref().ok());
        r
    }))
}

fn resolve_inner(name: &str) -> Option<HostFn> {
    match name {
        // ---- ウィンドウ ----
        "RegisterClassA" => boxed(|e| {
            let wc = a(e, 1)? as u64;
            let wndproc = e.mem.read_n(wc + 4, 4)? as u32;
            let bg = e.mem.read_n(wc + 28, 4)? as u32;
            let cname = e.mem.read_cstr(e.mem.read_n(wc + 36, 4)?, 128)?;
            let atom = 0xc000 + e.gui.classes.len() as u32;
            e.gui.classes.insert(cname, crate::gui::Class { atom, wndproc, bg });
            Ok(atom as u64)
        }),
        "UnregisterClassA" => boxed(|_| Ok(1)),
        "CreateWindowExA" => boxed(|e| {
            let cptr = a(e, 2)?;
            let cname = if cptr < 0x1_0000 { e.gui.classes.iter().find(|(_, c)| c.atom == cptr).map(|(n, _)| n.clone()).unwrap_or_default() } else { e.mem.read_cstr(cptr as u64, 128)? };
            let (x, y, w, h) = (i(e, 5)?, i(e, 6)?, i(e, 7)?, i(e, 8)?);
            // CW_USEDEFAULT（0x80000000）は 0 にする
            let fix = |v: i32| if v == i32::MIN { 0 } else { v };
            let parent = a(e, 9)?;
            let bg = e.gui.classes.get(&cname).map(|c| c.bg).map(|b| if b == 0 { 0xd4d0c8 } else { brush_color(e, b).unwrap_or(0xd4d0c8) }).unwrap_or(0xd4d0c8);
            let hwnd = e.gui.add_window(&cname, parent, fix(x), fix(y), fix(w), fix(h), bg);
            // CREATESTRUCT（12 個の値）を作り、WM_NCCREATE、WM_CREATE を送る
            let cs = e.mem.alloc32(48);
            let vals = [a(e, 12)?, a(e, 11)?, a(e, 10)?, parent, h as u32, w as u32, y as u32, x as u32, a(e, 4)?, a(e, 3)?, cptr, a(e, 1)?];
            for (k, v) in vals.iter().enumerate() {
                e.mem.write_n(cs as u64 + k as u64 * 4, 4, *v as u64)?;
            }
            if e.send_message(hwnd, 0x81, 0, cs)? == 0 && e.gui.classes.get(&cname).is_some_and(|c| c.wndproc != 0) {
                e.gui.remove_window(hwnd);
                return Ok(0);
            }
            if e.send_message(hwnd, 0x01, 0, cs)? as u32 == u32::MAX {
                e.gui.remove_window(hwnd);
                return Ok(0);
            }
            Ok(hwnd as u64)
        }),
        "DestroyWindow" => boxed(|e| {
            let h = a(e, 1)?;
            let _ = e.send_message(h, crate::gui::WM_DESTROY, 0, 0);
            e.gui.remove_window(h);
            Ok(1)
        }),
        "DefWindowProcA" => boxed(|e| Ok(matches!(a(e, 2)?, 0x81 | 0x14) as u64)),
        "GetWindowLongA" => boxed(|e| {
            let (h, idx) = (a(e, 1)?, i(e, 2)?);
            let base = e.gui.base as u32;
            let Some(w) = e.gui.windows.get(&h) else { return Ok(0) };
            Ok(match w.longs.get(&idx) {
                Some(v) => *v as u64,
                None if idx == -4 => e.gui.classes.get(&w.class).map(|c| c.wndproc).unwrap_or(0) as u64,
                None if idx == -6 => base as u64,
                None => 0,
            })
        }),
        "SetWindowLongA" => boxed(|e| {
            let (h, idx, v) = (a(e, 1)?, i(e, 2)?, a(e, 3)?);
            let cls = e.gui.windows.get(&h).and_then(|w| e.gui.classes.get(&w.class)).map(|c| c.wndproc).unwrap_or(0);
            let Some(w) = e.gui.windows.get_mut(&h) else { return Ok(0) };
            let old = w.longs.insert(idx, v).unwrap_or(if idx == -4 { cls } else { 0 });
            Ok(old as u64)
        }),
        "GetParent" => boxed(|e| Ok(e.gui.windows.get(&a(e, 1)?).map(|w| w.parent).unwrap_or(0) as u64)),
        "GetWindowRect" => boxed(|e| {
            let (h, p) = (a(e, 1)?, a(e, 2)? as u64);
            let Some(w) = e.gui.windows.get(&h) else { return Ok(0) };
            let (ox, oy) = e.gui.origin(h);
            for (k, v) in [ox, oy, ox + w.w, oy + w.h].iter().enumerate() {
                e.mem.write_n(p + k as u64 * 4, 4, *v as u32 as u64)?;
            }
            Ok(1)
        }),
        "MapWindowPoints" => boxed(|e| {
            let (from, to, pts, n) = (a(e, 1)?, a(e, 2)?, a(e, 3)? as u64, a(e, 4)? as u64);
            let (fo, to_o) = (e.gui.origin(from), e.gui.origin(to));
            let (dx, dy) = (fo.0 - to_o.0, fo.1 - to_o.1);
            for k in 0..n {
                for (o, d) in [(0u64, dx), (4, dy)] {
                    let v = e.mem.read_n(pts + k * 8 + o, 4)? as u32 as i32;
                    e.mem.write_n(pts + k * 8 + o, 4, (v + d) as u32 as u64)?;
                }
            }
            Ok((((dy as u32) << 16) | (dx as u32 & 0xffff)) as u64)
        }),
        "PeekMessageA" | "DispatchMessageA" | "SetCursor" => boxed(|_| Ok(0)),
        "GetDoubleClickTime" => boxed(|_| Ok(500)),
        "LoadCursorA" => boxed(|_| Ok(1)),
        "ShowCursor" => boxed(|e| {
            e.gui.cursor_count += if a(e, 1)? != 0 { 1 } else { -1 };
            Ok(e.gui.cursor_count as u32 as u64)
        }),
        "GetCursorPos" => boxed(|e| {
            let p = a(e, 1)? as u64;
            e.mem.write_n(p, 4, e.gui.cursor.0 as u32 as u64)?;
            e.mem.write_n(p + 4, 4, e.gui.cursor.1 as u32 as u64)?;
            Ok(1)
        }),
        "SetCursorPos" => boxed(|e| {
            e.gui.cursor = (i(e, 1)?, i(e, 2)?);
            Ok(1)
        }),
        "GetAsyncKeyState" => boxed(|e| {
            let vk = a(e, 1)?;
            // 左ボタンを調べるのは、押している間ずっと待つループ（つまみのドラッグ）。ここで中断して、次の入力を待つ
            if vk == 1 && e.gui.can_yield {
                e.yield_req = true;
            }
            let bit = match vk {
                0x01 => 0,
                0x11 | 0xa2 | 0xa3 => 1,
                0x10 | 0xa0 | 0xa1 => 2,
                0x12 => 3,
                _ => return Ok(0),
            };
            Ok(if e.gui.keys >> bit & 1 == 1 { 0x8000 } else { 0 })
        }),
        "GetSystemMetrics" => boxed(|e| {
            Ok(match a(e, 1)? {
                0 => 1920,
                1 => 1080,
                4 | 15 => 19,
                5 | 6 => 1,
                7 | 8 => 3,
                32 | 33 => 4,
                _ => 0,
            })
        }),
        // ---- 描画の面（DC） ----
        "GetDC" => boxed(|e| {
            let h = a(e, 1)?;
            let surface = match e.gui.windows.get(&h) {
                Some(w) => w.surface,
                None => e.gui.gdi.add(Obj::Bitmap(Bitmap::new(1, 1, 0))),
            };
            Ok(e.gui.gdi.new_dc(surface) as u64)
        }),
        "ReleaseDC" | "DeleteDC" => boxed(|e| {
            let h = if a(e, 1)? < 0x0100_0000 && e.gui.gdi.dc(a(e, 2)?).is_some() { a(e, 2)? } else { a(e, 1)? };
            e.gui.gdi.objs.remove(&h);
            Ok(1)
        }),
        "BeginPaint" => boxed(|e| {
            let (h, ps) = (a(e, 1)?, a(e, 2)? as u64);
            let Some(w) = e.gui.windows.get(&h) else { return Ok(0) };
            let (surface, ww, wh) = (w.surface, w.w, w.h);
            let dc = e.gui.gdi.new_dc(surface);
            e.mem.write(ps, &[0u8; 64])?;
            e.mem.write_n(ps, 4, dc as u64)?;
            e.mem.write_n(ps + 16, 4, ww as u64)?;
            e.mem.write_n(ps + 20, 4, wh as u64)?;
            Ok(dc as u64)
        }),
        "EndPaint" => boxed(|e| {
            let ps = a(e, 2)? as u64;
            let dc = e.mem.read_n(ps, 4)? as u32;
            e.gui.gdi.objs.remove(&dc);
            Ok(1)
        }),
        "GetUpdateRect" => boxed(|e| {
            let (h, p) = (a(e, 1)?, a(e, 2)? as u64);
            let Some(w) = e.gui.windows.get(&h) else { return Ok(0) };
            if p != 0 {
                for (k, v) in [0, 0, w.w, w.h].iter().enumerate() {
                    e.mem.write_n(p + k as u64 * 4, 4, *v as u64)?;
                }
            }
            Ok(1)
        }),
        "FillRect" => boxed(|e| {
            let (dc, (l, t, r, b)) = (a(e, 1)?, rect(e, a(e, 2)?)?);
            if let Some(rgb) = brush_color(e, a(e, 3)?) {
                e.gui.gdi.fill(dc, l, t, r, b, rgb);
            }
            Ok(1)
        }),
        "GetSysColorBrush" => boxed(|e| {
            let c = sys_color(a(e, 1)?);
            Ok(e.gui.gdi.add(Obj::Brush { rgb: c, hollow: false }) as u64)
        }),
        "DrawTextA" => boxed(|e| {
            let (dc, tp, len, rp, fmt) = (a(e, 1)?, a(e, 2)? as u64, i(e, 3)?, a(e, 4)?, a(e, 5)?);
            let text = if len < 0 { e.mem.read_cstr(tp, 1024)?.into_bytes() } else { let mut b = vec![0u8; len as usize]; e.mem.read(tp, &mut b)?; b };
            let (l, t, r, b) = rect(e, rp)?;
            let (tw, th) = e.gui.gdi.text(dc, 0, 0, &text, false);
            // DT_CENTER=1、DT_RIGHT=2、DT_VCENTER=4、DT_BOTTOM=8、DT_CALCRECT=0x400
            let x = if fmt & 1 != 0 { l + (r - l - tw) / 2 } else if fmt & 2 != 0 { r - tw } else { l };
            let y = if fmt & 4 != 0 { t + (b - t - th) / 2 } else if fmt & 8 != 0 { b - th } else { t };
            if fmt & 0x400 != 0 {
                e.mem.write_n(rp as u64 + 8, 4, (l + tw) as u32 as u64)?;
                e.mem.write_n(rp as u64 + 12, 4, (t + th) as u32 as u64)?;
            } else {
                e.gui.gdi.text(dc, x, y, &text, true);
            }
            Ok(th as u64)
        }),
        "LoadBitmapA" => boxed(|e| {
            let name = res_name(e, a(e, 2)?)?;
            let Some(dib) = rsrc::find_bitmap(&e.mem, e.gui.base, &name)? else { return Ok(0) };
            Ok(rsrc::decode_dib(&dib).map(|b| e.gui.gdi.add(Obj::Bitmap(b))).unwrap_or(0) as u64)
        }),
        // ---- GDI のオブジェクト ----
        "GetStockObject" => boxed(|e| Ok((STOCK_BASE + a(e, 1)?) as u64)),
        "CreateSolidBrush" => boxed(|e| Ok(e.gui.gdi.add(Obj::Brush { rgb: swap_rb(a(e, 1)?), hollow: false }) as u64)),
        "CreateBrushIndirect" => boxed(|e| {
            let p = a(e, 1)? as u64;
            let (style, color) = (e.mem.read_n(p, 4)?, e.mem.read_n(p + 4, 4)? as u32);
            Ok(e.gui.gdi.add(Obj::Brush { rgb: swap_rb(color), hollow: style == 1 }) as u64)
        }),
        "CreatePenIndirect" => boxed(|e| {
            let p = a(e, 1)? as u64;
            let (style, width, color) = (e.mem.read_n(p, 4)?, e.mem.read_n(p + 4, 4)? as u32 as i32, e.mem.read_n(p + 12, 4)? as u32);
            Ok(e.gui.gdi.add(Obj::Pen { rgb: swap_rb(color), width, null: style == 5 }) as u64)
        }),
        "CreateFontIndirectA" => boxed(|e| {
            let height = e.mem.read_n(a(e, 1)? as u64, 4)? as u32 as i32;
            Ok(e.gui.gdi.add(Obj::Font { height }) as u64)
        }),
        "CreateCompatibleDC" => boxed(|e| {
            let bm = e.gui.gdi.add(Obj::Bitmap(Bitmap::new(1, 1, 0)));
            Ok(e.gui.gdi.new_dc(bm) as u64)
        }),
        "CreateCompatibleBitmap" => boxed(|e| Ok(e.gui.gdi.add(Obj::Bitmap(Bitmap::new(i(e, 2)?, i(e, 3)?, 0))) as u64)),
        "CreateBitmap" => boxed(|e| {
            let (w, h, bpp, bits) = (i(e, 1)?, i(e, 2)?, a(e, 4)?, a(e, 5)? as u64);
            let mut bm = Bitmap::new(w, h, 0);
            if bits != 0 && bm.w > 0 {
                // 行は 16 ビット（WORD）の境界。1 ビットは 1 が白
                let stride = ((bm.w as u64 * bpp as u64).div_ceil(16) * 2) as usize;
                let mut buf = vec![0u8; stride * bm.h as usize];
                e.mem.read(bits, &mut buf)?;
                for y in 0..bm.h as usize {
                    for x in 0..bm.w as usize {
                        let line = &buf[y * stride..];
                        bm.px[y * bm.w as usize + x] = match bpp {
                            1 => if (line[x / 8] >> (7 - x % 8)) & 1 == 1 { 0xffffff } else { 0 },
                            24 | 32 => {
                                let p = x * bpp as usize / 8;
                                (line[p + 2] as u32) << 16 | (line[p + 1] as u32) << 8 | line[p] as u32
                            }
                            _ => 0,
                        };
                    }
                }
            }
            Ok(e.gui.gdi.add(Obj::Bitmap(bm)) as u64)
        }),
        "DeleteObject" => boxed(|e| {
            let h = a(e, 1)?;
            if !(STOCK_BASE..STOCK_BASE + 32).contains(&h) {
                e.gui.gdi.objs.remove(&h);
            }
            Ok(1)
        }),
        "SelectObject" => boxed(|e| {
            let (dc, obj) = (a(e, 1)?, a(e, 2)?);
            let kind = match e.gui.gdi.objs.get(&obj) {
                Some(Obj::Bitmap(_)) => 0,
                Some(Obj::Brush { .. }) => 1,
                Some(Obj::Pen { .. }) => 2,
                Some(Obj::Font { .. }) => 3,
                _ => return Ok(0),
            };
            let Some(d) = e.gui.gdi.dc_mut(dc) else { return Ok(0) };
            let slot = match kind {
                0 => &mut d.bitmap,
                1 => &mut d.brush,
                2 => &mut d.pen,
                _ => &mut d.font,
            };
            Ok(std::mem::replace(slot, obj) as u64)
        }),
        "GetCurrentObject" => boxed(|e| {
            let (dc, ty) = (a(e, 1)?, a(e, 2)?);
            Ok(e.gui.gdi.dc(dc).map(|d| match ty {
                1 => d.pen,
                2 => d.brush,
                6 => d.font,
                7 => d.bitmap,
                _ => 0,
            }).unwrap_or(0) as u64)
        }),
        "GetObjectA" => boxed(|e| {
            let (h, p) = (a(e, 1)?, a(e, 3)? as u64);
            let Some(b) = e.gui.gdi.bitmap(h) else { return Ok(0) };
            // BITMAP: type、幅、高さ、1 行のバイト数、planes(2)、bpp(2)、bits
            let (w, hh) = (b.w, b.h);
            e.mem.write_n(p, 4, 0)?;
            e.mem.write_n(p + 4, 4, w as u64)?;
            e.mem.write_n(p + 8, 4, hh as u64)?;
            e.mem.write_n(p + 12, 4, w as u64 * 4)?;
            e.mem.write_n(p + 16, 2, 1)?;
            e.mem.write_n(p + 18, 2, 32)?;
            e.mem.write_n(p + 20, 4, 0)?;
            Ok(24)
        }),
        "DPtoLP" => boxed(|_| Ok(1)),
        // MSIMG32: 指定の色を抜いて写す、半透明で重ねる（画素ごとのアルファは扱わず、全体のアルファだけ）
        "TransparentBlt" | "AlphaBlend" => {
            let alpha = name == "AlphaBlend";
            boxed(move |e| {
                let dc = a(e, 1)?;
                let dr = (i(e, 2)?, i(e, 3)?, i(e, 4)?, i(e, 5)?);
                let sr = (i(e, 7)?, i(e, 8)?, i(e, 9)?, i(e, 10)?);
                let last = a(e, 11)?;
                // AlphaBlend の BLENDFUNCTION は 4 バイト（BlendOp、BlendFlags、SourceConstantAlpha、AlphaFormat）
                let (keyed, al) = if alpha { (None, (last >> 16) & 0xff) } else { (Some(swap_rb(last)), 255) };
                e.gui.gdi.stretch(dc, dr, a(e, 6)?, sr, keyed, al);
                Ok(1)
            })
        }
        "MoveToEx" => boxed(|e| {
            let (dc, x, y, old) = (a(e, 1)?, i(e, 2)?, i(e, 3)?, a(e, 4)? as u64);
            let Some(d) = e.gui.gdi.dc_mut(dc) else { return Ok(0) };
            let prev = std::mem::replace(&mut d.pos, (x, y));
            if old != 0 {
                e.mem.write_n(old, 4, prev.0 as u32 as u64)?;
                e.mem.write_n(old + 4, 4, prev.1 as u32 as u64)?;
            }
            Ok(1)
        }),
        "LineTo" => boxed(|e| {
            let (dc, x, y) = (a(e, 1)?, i(e, 2)?, i(e, 3)?);
            e.gui.gdi.line_to(dc, x, y);
            Ok(1)
        }),
        "BitBlt" => boxed(|e| {
            let (dst, dx, dy, w, h, src, sx, sy, rop) = (a(e, 1)?, i(e, 2)?, i(e, 3)?, i(e, 4)?, i(e, 5)?, a(e, 6)?, i(e, 7)?, i(e, 8)?, a(e, 9)?);
            e.gui.gdi.blit(dst, dx, dy, w, h, (src != 0).then_some(src), sx, sy, rop);
            Ok(1)
        }),
        _ => resolve_dc_state(name),
    }
}

/// 文字の色、背景の色、背景のモード（DC の設定を替え、前の値を返す）
pub fn resolve_dc_state(name: &str) -> Option<HostFn> {
    match name {
        "SetTextColor" => boxed(|e| {
            let (dc, c) = (a(e, 1)?, swap_rb(a(e, 2)?));
            Ok(e.gui.gdi.dc_mut(dc).map(|d| swap_rb(std::mem::replace(&mut d.text, c))).unwrap_or(0) as u64)
        }),
        "SetBkColor" => boxed(|e| {
            let (dc, c) = (a(e, 1)?, swap_rb(a(e, 2)?));
            Ok(e.gui.gdi.dc_mut(dc).map(|d| swap_rb(std::mem::replace(&mut d.bk, c))).unwrap_or(0) as u64)
        }),
        "SetBkMode" => boxed(|e| {
            let (dc, m) = (a(e, 1)?, a(e, 2)?);
            Ok(e.gui.gdi.dc_mut(dc).map(|d| std::mem::replace(&mut d.bk_mode, m)).unwrap_or(0) as u64)
        }),
        _ => None,
    }
}
