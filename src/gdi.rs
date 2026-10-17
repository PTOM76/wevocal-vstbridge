//! GDI の描画の部分。ビットマップ（0xRRGGBB の画素）、ペン、ブラシ、フォントを番号（ハンドル）で持ち、線、塗り、BitBlt、文字を描く。
//! 描く先は全部メモリ上のビットマップで、画面（canvas）に出すのは gui.rs
use std::collections::HashMap;

/// 画素の並び（0x00RRGGBB）。幅 w、高さ h
#[derive(Clone)]
pub struct Bitmap {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u32>,
}

impl Bitmap {
    pub fn new(w: i32, h: i32, fill: u32) -> Bitmap {
        let (w, h) = (w.clamp(0, 4096), h.clamp(0, 4096));
        Bitmap { w, h, px: vec![fill; (w * h) as usize] }
    }
    pub fn get(&self, x: i32, y: i32) -> Option<u32> {
        (x >= 0 && y >= 0 && x < self.w && y < self.h).then(|| self.px[(y * self.w + x) as usize])
    }
    pub fn set(&mut self, x: i32, y: i32, c: u32) {
        if x >= 0 && y >= 0 && x < self.w && y < self.h {
            self.px[(y * self.w + x) as usize] = c;
        }
    }
}

/// COLORREF（0x00BBGGRR）→ 0x00RRGGBB（と逆。同じ入れ替え）
pub fn swap_rb(c: u32) -> u32 {
    ((c & 0xff) << 16) | (c & 0xff00) | ((c >> 16) & 0xff)
}

#[derive(Clone)]
pub struct Dc {
    /// 描く先のビットマップのハンドル
    pub bitmap: u32,
    pub brush: u32,
    pub pen: u32,
    pub font: u32,
    pub text: u32,
    pub bk: u32,
    /// 背景を塗るか（OPAQUE=2。TRANSPARENT=1 なら文字の裏を塗らない）
    pub bk_mode: u32,
    pub pos: (i32, i32),
}

pub enum Obj {
    Bitmap(Bitmap),
    Brush { rgb: u32, hollow: bool },
    Pen { rgb: u32, width: i32, null: bool },
    Font { height: i32 },
    Dc(Dc),
}

#[derive(Default)]
pub struct Gdi {
    pub objs: HashMap<u32, Obj>,
    next: u32,
}

/// 標準のオブジェクト（GetStockObject）のハンドル
pub const STOCK_BASE: u32 = 0x0190_0000;

impl Gdi {
    pub fn new() -> Gdi {
        let mut g = Gdi { objs: HashMap::new(), next: 0x0200_0000 };
        // WHITE_BRUSH 0、LTGRAY 1、GRAY 2、DKGRAY 3、BLACK 4、NULL 5、WHITE_PEN 6、BLACK_PEN 7、NULL_PEN 8
        for (i, rgb) in [0xffffff, 0xc0c0c0, 0x808080, 0x404040, 0x000000].into_iter().enumerate() {
            g.objs.insert(STOCK_BASE + i as u32, Obj::Brush { rgb, hollow: false });
        }
        g.objs.insert(STOCK_BASE + 5, Obj::Brush { rgb: 0, hollow: true });
        g.objs.insert(STOCK_BASE + 6, Obj::Pen { rgb: 0xffffff, width: 1, null: false });
        g.objs.insert(STOCK_BASE + 7, Obj::Pen { rgb: 0, width: 1, null: false });
        g.objs.insert(STOCK_BASE + 8, Obj::Pen { rgb: 0, width: 1, null: true });
        // SYSTEM_FONT 13、DEFAULT_GUI_FONT 17 など
        for i in 10..=17 {
            g.objs.insert(STOCK_BASE + i, Obj::Font { height: 13 });
        }
        g
    }

    pub fn add(&mut self, o: Obj) -> u32 {
        self.next += 4;
        self.objs.insert(self.next, o);
        self.next
    }

    pub fn new_dc(&mut self, bitmap: u32) -> u32 {
        self.add(Obj::Dc(Dc { bitmap, brush: STOCK_BASE, pen: STOCK_BASE + 7, font: STOCK_BASE + 13, text: 0, bk: 0xffffff, bk_mode: 2, pos: (0, 0) }))
    }

    pub fn dc(&self, h: u32) -> Option<&Dc> {
        match self.objs.get(&h) {
            Some(Obj::Dc(d)) => Some(d),
            _ => None,
        }
    }
    pub fn dc_mut(&mut self, h: u32) -> Option<&mut Dc> {
        match self.objs.get_mut(&h) {
            Some(Obj::Dc(d)) => Some(d),
            _ => None,
        }
    }
    pub fn bitmap(&self, h: u32) -> Option<&Bitmap> {
        match self.objs.get(&h) {
            Some(Obj::Bitmap(b)) => Some(b),
            _ => None,
        }
    }
    pub fn bitmap_mut(&mut self, h: u32) -> Option<&mut Bitmap> {
        match self.objs.get_mut(&h) {
            Some(Obj::Bitmap(b)) => Some(b),
            _ => None,
        }
    }

    /// DC の描く先のビットマップ
    fn target(&mut self, dc: u32) -> Option<(&mut Bitmap, Dc)> {
        let d = self.dc(dc)?.clone();
        let b = self.bitmap_mut(d.bitmap)?;
        Some((b, d))
    }

    pub fn brush_rgb(&self, h: u32) -> Option<u32> {
        match self.objs.get(&h) {
            Some(Obj::Brush { rgb, hollow: false }) => Some(*rgb),
            _ => None,
        }
    }

    /// 矩形を塗る（right、bottom は含まない）
    pub fn fill(&mut self, dc: u32, l: i32, t: i32, r: i32, b: i32, rgb: u32) {
        let Some((bm, _)) = self.target(dc) else { return };
        for y in t.max(0)..b.min(bm.h) {
            for x in l.max(0)..r.min(bm.w) {
                bm.px[(y * bm.w + x) as usize] = rgb;
            }
        }
    }

    /// 現在位置から (x, y) まで線を引き、現在位置を動かす（最後の点は含めない。LineTo の決まり）
    pub fn line_to(&mut self, dc: u32, x: i32, y: i32) {
        let pen = match self.dc(dc).map(|d| d.pen).and_then(|p| self.objs.get(&p)) {
            Some(Obj::Pen { rgb, width, null }) => (*rgb, (*width).max(1), *null),
            _ => (0, 1, false),
        };
        let Some((bm, d)) = self.target(dc) else { return };
        let (mut x0, mut y0) = d.pos;
        let (dx, dy) = ((x - x0).abs(), -(y - y0).abs());
        let (sx, sy) = (if x0 < x { 1 } else { -1 }, if y0 < y { 1 } else { -1 });
        let mut err = dx + dy;
        if !pen.2 {
            while (x0, y0) != (x, y) {
                for oy in 0..pen.1 {
                    for ox in 0..pen.1 {
                        bm.set(x0 + ox - pen.1 / 2, y0 + oy - pen.1 / 2, pen.0);
                    }
                }
                let e2 = 2 * err;
                if e2 >= dy {
                    err += dy;
                    x0 += sx;
                }
                if e2 <= dx {
                    err += dx;
                    y0 += sy;
                }
            }
        }
        if let Some(d) = self.dc_mut(dc) {
            d.pos = (x, y);
        }
    }

    /// BitBlt。rop は 32 ビットのラスターオペレーション（SRCCOPY=0xCC0020 など）。主な種類だけ扱い、残りは SRCCOPY と同じ
    #[allow(clippy::too_many_arguments)]
    pub fn blit(&mut self, dst: u32, dx: i32, dy: i32, w: i32, h: i32, src: Option<u32>, sx: i32, sy: i32, rop: u32) {
        let brush = self.dc(dst).and_then(|d| self.brush_rgb(d.brush)).unwrap_or(0);
        // 元を先に写す（同じビットマップの重なりでも壊れないように）
        let mut from: Vec<(i32, i32, u32)> = Vec::new();
        if let Some(sd) = src.and_then(|s| self.dc(s)).map(|d| d.bitmap) {
            if let Some(sb) = self.bitmap(sd) {
                for j in 0..h {
                    for i in 0..w {
                        if let Some(c) = sb.get(sx + i, sy + j) {
                            from.push((i, j, c));
                        }
                    }
                }
            }
        }
        let Some((bm, _)) = self.target(dst) else { return };
        let op = |d: u32, s: u32| -> u32 {
            (match rop {
                0x00CC0020 => s,
                0x008800C6 => d & s,
                0x00EE0086 => d | s,
                0x00660046 => d ^ s,
                0x00330008 => !s,
                0x00220326 => d & !s,
                0x00BB0226 => d | !s,
                0x00000042 => 0,
                0x00FF0062 => 0xffffff,
                0x00F00021 => brush,
                0x00550009 => !d,
                _ => s,
            }) & 0xffffff
        };
        // 元を使わない種類（黒、白、ブラシ、反転）
        if matches!(rop, 0x42 | 0xFF0062 | 0xF00021 | 0x550009) || src.is_none() {
            for j in 0..h {
                for i in 0..w {
                    if let Some(d) = bm.get(dx + i, dy + j) {
                        bm.set(dx + i, dy + j, op(d, 0));
                    }
                }
            }
        } else {
            for (i, j, s) in from {
                if let Some(d) = bm.get(dx + i, dy + j) {
                    bm.set(dx + i, dy + j, op(d, s));
                }
            }
        }
    }

    /// 文字を描く（8x8 のビットマップのフォントを、フォントの高さに合わせて拡大縮小する。ASCII だけ）。描いた幅と高さを返す
    pub fn text(&mut self, dc: u32, x: i32, y: i32, s: &[u8], draw: bool) -> (i32, i32) {
        let font_h = match self.dc(dc).map(|d| d.font).and_then(|f| self.objs.get(&f)) {
            Some(Obj::Font { height }) => height.abs().max(6),
            _ => 13,
        };
        let adv = (font_h * 3 / 4).max(4);
        if draw {
            if let Some((bm, d)) = self.target(dc) {
                for (n, &ch) in s.iter().enumerate() {
                    let glyph = font8x8::legacy::BASIC_LEGACY[(ch & 0x7f) as usize];
                    let gx0 = x + n as i32 * adv;
                    for py in 0..font_h {
                        for px in 0..adv {
                            let on = (glyph[(py * 8 / font_h) as usize] >> (px * 8 / adv)) & 1 == 1;
                            if on {
                                bm.set(gx0 + px, y + py, d.text);
                            } else if d.bk_mode == 2 {
                                bm.set(gx0 + px, y + py, d.bk);
                            }
                        }
                    }
                }
            }
        }
        (adv * s.len() as i32, font_h)
    }

    /// 元の矩形を、先の矩形の大きさに（最近傍で）拡大縮小しながら写す。keyed が Some なら、その色の画素は写さない。alpha（0〜255）で混ぜる
    #[allow(clippy::too_many_arguments)]
    pub fn stretch(&mut self, dst: u32, dr: (i32, i32, i32, i32), src: u32, sr: (i32, i32, i32, i32), keyed: Option<u32>, alpha: u32) {
        let Some(sb) = self.dc(src).map(|d| d.bitmap).and_then(|b| self.bitmap(b)).cloned() else { return };
        let Some((bm, _)) = self.target(dst) else { return };
        let (dx, dy, dw, dh) = dr;
        let (sx, sy, sw, sh) = sr;
        if dw == 0 || dh == 0 || sw == 0 || sh == 0 {
            return;
        }
        for j in 0..dh.abs() {
            for i in 0..dw.abs() {
                let (u, v) = (sx + i * sw.abs() / dw.abs(), sy + j * sh.abs() / dh.abs());
                let Some(c) = sb.get(u, v) else { continue };
                if keyed == Some(c) {
                    continue;
                }
                let Some(d) = bm.get(dx + i, dy + j) else { continue };
                let mix = |s: u32, d: u32| (s * alpha + d * (255 - alpha)) / 255;
                let out = if alpha >= 255 {
                    c
                } else {
                    mix((c >> 16) & 0xff, (d >> 16) & 0xff) << 16 | mix((c >> 8) & 0xff, (d >> 8) & 0xff) << 8 | mix(c & 0xff, d & 0xff)
                };
                bm.set(dx + i, dy + j, out);
            }
        }
    }
}
