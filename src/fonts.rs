//! プラグインが AddFontMemResourceEx で渡すフォント（TrueType、OpenType）を覚え、GDI の文字の API（GetGlyphIndicesW、GetGlyphOutlineW、GetTextMetrics）に答える。
//! 字形は Windows の GGO_NATIVE と同じ形（TTPOLYGONHEADER と TTPOLYCURVE。直線と 2 次曲線）で返す。JUCE が自分で塗る
use crate::emu::{Emu, HostFn};
use crate::gdi::Obj;
use crate::host_stubs::{arg, read_wstr};
use crate::mem::R;
use std::collections::HashMap;

/// 覚えたフォント（ファミリー名, データ）と、CreateFontIndirect で作ったフォントの名前
#[derive(Default)]
pub struct Fonts {
    pub data: Vec<(String, Vec<u8>)>,
    pub faces: HashMap<u32, String>,
}

/// ファミリー名（name の 16 番、なければ 1 番）
fn family(data: &[u8]) -> Option<String> {
    let face = ttf_parser::Face::parse(data, 0).ok()?;
    let names = face.names();
    let pick = |id| names.into_iter().filter(|n| n.name_id == id && n.is_unicode()).find_map(|n| n.to_string());
    pick(16).or_else(|| pick(1))
}

/// dc で選ばれているフォントの (データの添字, 1 em のピクセル数)。名前が合わなければ最初のフォント
fn dc_font(e: &Emu, dc: u32) -> Option<(usize, f64)> {
    let h = e.gui.gdi.dc(dc)?.font;
    let height = match e.gui.gdi.objs.get(&h) {
        Some(Obj::Font { height }) => *height,
        _ => 13,
    };
    let name = e.gui.fonts.faces.get(&h);
    let k = name.and_then(|n| e.gui.fonts.data.iter().position(|(f, _)| f.eq_ignore_ascii_case(n))).or((!e.gui.fonts.data.is_empty()).then_some(0))?;
    let face = ttf_parser::Face::parse(&e.gui.fonts.data[k].1, 0).ok()?;
    // 負の高さは em の大きさ、正の高さは行の高さ（上と下の和）
    let em = if height < 0 {
        -height as f64
    } else {
        let (asc, desc) = win_metrics(&face);
        height.max(1) as f64 * face.units_per_em() as f64 / (asc + desc) as f64
    };
    Some((k, em))
}

/// Windows が行の高さに使う上と下（OS/2 の usWinAscent、usWinDescent。なければ hhea）
fn win_metrics(face: &ttf_parser::Face) -> (i32, i32) {
    match face.tables().os2 {
        Some(os2) if os2.windows_ascender() > 0 => (os2.windows_ascender() as i32, os2.windows_descender().unsigned_abs() as i32),
        _ => (face.ascender() as i32, -(face.descender() as i32)),
    }
}

/// 字形を GDI の形の点の列に直す（y は上向き、単位はピクセル）
struct Native {
    out: Vec<u8>,
    /// 今の輪郭の TTPOLYGONHEADER の位置
    head: usize,
    scale: f64,
    last: (f32, f32),
}

fn fixed(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_le_bytes()
}

impl Native {
    fn pt(&mut self, x: f32, y: f32) {
        self.out.extend(fixed(x as f64 * self.scale));
        self.out.extend(fixed(y as f64 * self.scale));
    }
    fn curve(&mut self, ty: u16, pts: &[(f32, f32)]) {
        self.out.extend(ty.to_le_bytes());
        self.out.extend((pts.len() as u16).to_le_bytes());
        for &(x, y) in pts {
            self.pt(x, y);
        }
        self.last = *pts.last().unwrap();
    }
    fn finish(&mut self) {
        if self.head < self.out.len() {
            let cb = (self.out.len() - self.head) as u32;
            self.out[self.head..self.head + 4].copy_from_slice(&cb.to_le_bytes());
        }
    }
}

impl ttf_parser::OutlineBuilder for Native {
    fn move_to(&mut self, x: f32, y: f32) {
        self.finish();
        self.head = self.out.len();
        // cb（あとで入れる）、dwType = TT_POLYGON_TYPE（24）、pfxStart
        self.out.extend([0u8; 4]);
        self.out.extend(24u32.to_le_bytes());
        self.pt(x, y);
        self.last = (x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.curve(1, &[(x, y)]);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.curve(2, &[(x1, y1), (x, y)]);
    }
    /// 3 次曲線（CFF）は 4 つの 2 次曲線で近づける
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let p0 = self.last;
        let at = |t: f32| {
            let u = 1.0 - t;
            let f = |a: f32, b: f32, c: f32, d: f32| u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d;
            (f(p0.0, x1, x2, x), f(p0.1, y1, y2, y))
        };
        for k in 0..4 {
            let (t0, t1) = (k as f32 / 4.0, (k + 1) as f32 / 4.0);
            let (a, m, b) = (at(t0), at((t0 + t1) / 2.0), at(t1));
            // 中点を通る 2 次曲線の制御点
            let c = (2.0 * m.0 - (a.0 + b.0) / 2.0, 2.0 * m.1 - (a.1 + b.1) / 2.0);
            self.curve(2, &[c, b]);
        }
    }
    fn close(&mut self) {}
}

fn boxed(f: impl Fn(&mut Emu) -> R<u64> + 'static) -> Option<HostFn> {
    Some(Box::new(f))
}

pub fn resolve(name: &str) -> Option<HostFn> {
    match name {
        // AddFontMemResourceEx(data, size, 0, &count)
        "AddFontMemResourceEx" => boxed(|e| {
            let (p, n, cnt) = (arg(e, 1)?, arg(e, 2)? as usize, arg(e, 4)?);
            let mut data = vec![0u8; n];
            e.mem.read(p, &mut data)?;
            let Some(name) = family(&data) else { return Ok(0) };
            e.gui.fonts.data.push((name, data));
            if cnt != 0 {
                e.mem.write_n(cnt, 4, 1)?;
            }
            Ok(0x5000 + e.gui.fonts.data.len() as u64)
        }),
        "RemoveFontMemResourceEx" => boxed(|_| Ok(1)),
        // LOGFONTW: lfHeight（4）…、lfFaceName は 28 バイト目から（32 文字）
        "CreateFontIndirectW" => boxed(|e| {
            let p = arg(e, 1)?;
            let height = e.mem.read_n(p, 4)? as u32 as i32;
            let face = read_wstr(e, p + 28)?;
            let h = e.gui.gdi.add(Obj::Font { height });
            e.gui.fonts.faces.insert(h, face);
            Ok(h as u64)
        }),
        "GetKerningPairsW" | "GetKerningPairsA" => boxed(|_| Ok(0)),
        // GetGlyphIndicesW(dc, 文字列, 数, 添字の出力, fl)。fl の 1 は、無い字を 0xFFFF にする
        "GetGlyphIndicesW" => boxed(|e| {
            let (dc, s, n, out, fl) = (arg(e, 1)? as u32, arg(e, 2)?, arg(e, 3)?, arg(e, 4)?, arg(e, 5)?);
            let font = dc_font(e, dc);
            for k in 0..n {
                let c = e.mem.read_n(s + k * 2, 2)? as u32;
                let g = font.and_then(|(f, _)| ttf_parser::Face::parse(&e.gui.fonts.data[f].1, 0).ok()?.glyph_index(char::from_u32(c)?)).map(|g| g.0);
                let v = g.unwrap_or(if fl & 1 != 0 { 0xffff } else { 0 });
                e.mem.write_n(out + k * 2, 2, v as u64)?;
            }
            Ok(n)
        }),
        // GetGlyphOutlineW(dc, 文字, 形式, GLYPHMETRICS, 大きさ, 出力, 行列)。GGO_NATIVE（2）だけ。GGO_GLYPH_INDEX（0x80）なら文字は字形の添字
        "GetGlyphOutlineW" => boxed(|e| {
            let (dc, ch, fmt, gm, size, buf) = (arg(e, 1)? as u32, arg(e, 2)? as u32, arg(e, 3)? as u32, arg(e, 4)?, arg(e, 5)?, arg(e, 6)?);
            const GDI_ERROR: u64 = 0xffff_ffff;
            if fmt & 0x7f != 2 {
                return Ok(GDI_ERROR);
            }
            let Some((f, em)) = dc_font(e, dc) else { return Ok(GDI_ERROR) };
            let data = e.gui.fonts.data[f].1.clone();
            let Ok(face) = ttf_parser::Face::parse(&data, 0) else { return Ok(GDI_ERROR) };
            let gid = if fmt & 0x80 != 0 { Some(ttf_parser::GlyphId(ch as u16)) } else { char::from_u32(ch).and_then(|c| face.glyph_index(c)) };
            let Some(gid) = gid else { return Ok(GDI_ERROR) };
            let scale = em / face.units_per_em() as f64;
            let mut nat = Native { out: Vec::new(), head: 0, scale, last: (0.0, 0.0) };
            let bb = face.outline_glyph(gid, &mut nat);
            nat.finish();
            // GLYPHMETRICS: gmBlackBoxX、gmBlackBoxY、gmptGlyphOrigin（x、y）、gmCellIncX、gmCellIncY
            let adv = (face.glyph_hor_advance(gid).unwrap_or(0) as f64 * scale).round() as i32;
            let (bx, by, ox, oy) = match bb {
                Some(r) => (((r.x_max - r.x_min) as f64 * scale).ceil() as u32, ((r.y_max - r.y_min) as f64 * scale).ceil() as u32, (r.x_min as f64 * scale).floor() as i32, (r.y_max as f64 * scale).ceil() as i32),
                None => (1, 1, 0, 0),
            };
            if gm != 0 {
                for (k, v) in [bx, by, ox as u32, oy as u32].iter().enumerate() {
                    e.mem.write_n(gm + k as u64 * 4, 4, *v as u64)?;
                }
                e.mem.write_n(gm + 16, 2, adv as u16 as u64)?;
                e.mem.write_n(gm + 18, 2, 0)?;
            }
            if size == 0 || buf == 0 {
                return Ok(nat.out.len() as u64);
            }
            let n = nat.out.len().min(size as usize);
            e.mem.write(buf, &nat.out[..n])?;
            Ok(n as u64)
        }),
        // TEXTMETRIC（W は末尾の文字が 2 バイト）。覚えたフォントなら Windows と同じ上と下、なければ高さの 8 割と 2 割
        "GetTextMetricsW" | "GetTextMetricsA" => boxed(|e| {
            let (dc, p) = (arg(e, 1)? as u32, arg(e, 2)?);
            let font = dc_font(e, dc).and_then(|(f, em)| {
                let face = ttf_parser::Face::parse(&e.gui.fonts.data[f].1, 0).ok()?;
                let (asc, desc) = win_metrics(&face);
                let s = em / face.units_per_em() as f64;
                Some(((asc as f64 * s).round() as i32, (desc as f64 * s).round() as i32))
            });
            let (asc, desc) = font.unwrap_or_else(|| {
                let h = e.gui.gdi.dc(dc).and_then(|d| match e.gui.gdi.objs.get(&d.font) {
                    Some(Obj::Font { height }) => Some(height.abs().max(1)),
                    _ => None,
                }).unwrap_or(13);
                let asc = (h * 4 + 2) / 5;
                (asc, h - asc)
            });
            let h = asc + desc;
            e.mem.write(p, &[0u8; 60])?;
            // tmHeight、tmAscent、tmDescent、tmInternalLeading、tmExternalLeading、tmAveCharWidth、tmMaxCharWidth、tmWeight
            for (k, v) in [h, asc, desc, 0, 0, h / 2, h, 400].iter().enumerate() {
                e.mem.write_n(p + k as u64 * 4, 4, *v as u32 as u64)?;
            }
            Ok(1)
        }),
        _ => None,
    }
}
