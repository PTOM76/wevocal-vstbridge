//! 読み込んだ DLL のリソース（RT_BITMAP）を探して、ビットマップにする。LoadBitmapA が使う
use crate::gdi::Bitmap;
use crate::mem::{Memory, R};

/// リソースの名前（数字の ID か、文字列）
pub enum ResName {
    Id(u32),
    Name(String),
}

fn u32_at(mem: &Memory, a: u64) -> R<u32> {
    mem.read_n(a, 4).map(|v| v as u32)
}

/// ディレクトリ（dir から始まる）の中で name に合う項目の「次の位置」を返す。サブディレクトリなら (位置, true)
fn find_entry(mem: &Memory, rsrc: u64, dir: u64, name: Option<&ResName>) -> R<Option<(u64, bool)>> {
    let named = mem.read_n(dir + 12, 2)? as u64;
    let ids = mem.read_n(dir + 14, 2)? as u64;
    for i in 0..named + ids {
        let e = dir + 16 + i * 8;
        let key = u32_at(mem, e)?;
        let off = u32_at(mem, e + 4)?;
        let ok = match name {
            None => true,
            Some(ResName::Id(id)) => key & 0x8000_0000 == 0 && key == *id,
            Some(ResName::Name(n)) => {
                if key & 0x8000_0000 == 0 {
                    false
                } else {
                    let s = rsrc + (key & 0x7fff_ffff) as u64;
                    let len = mem.read_n(s, 2)? as u64;
                    let mut chars = String::new();
                    for k in 0..len {
                        chars.push(char::from_u32(mem.read_n(s + 2 + k * 2, 2)? as u32).unwrap_or('?'));
                    }
                    chars.eq_ignore_ascii_case(n)
                }
            }
        };
        if ok {
            return Ok(Some((rsrc + (off & 0x7fff_ffff) as u64, off & 0x8000_0000 != 0)));
        }
    }
    Ok(None)
}

/// base に読み込まれたイメージの RT_BITMAP（2）の name を探して、DIB（BITMAPINFOHEADER から）の中身を返す
pub fn find_bitmap(mem: &Memory, base: u64, name: &ResName) -> R<Option<Vec<u8>>> {
    let pe = base + u32_at(mem, base + 0x3c)? as u64;
    // 32 ビットの PE: オプションヘッダーは pe + 24、データディレクトリは +96。リソースは 3 番目（添字 2）
    let rva = u32_at(mem, pe + 24 + 96 + 16)? as u64;
    if rva == 0 {
        return Ok(None);
    }
    let rsrc = base + rva;
    let Some((ty, true)) = find_entry(mem, rsrc, rsrc, Some(&ResName::Id(2)))? else { return Ok(None) };
    let Some((nm, true)) = find_entry(mem, rsrc, ty, Some(name))? else { return Ok(None) };
    // 言語は最初のものを使う
    let Some((data, false)) = find_entry(mem, rsrc, nm, None)? else { return Ok(None) };
    let (data_rva, size) = (u32_at(mem, data)? as u64, u32_at(mem, data + 4)? as usize);
    let mut buf = vec![0u8; size];
    mem.read(base + data_rva, &mut buf)?;
    Ok(Some(buf))
}

/// DIB（BITMAPINFOHEADER、パレット、画素）を Bitmap にする。1、4、8、24、32 ビットに対応
pub fn decode_dib(d: &[u8]) -> Option<Bitmap> {
    let le32 = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let le16 = |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes(b.try_into().unwrap()));
    let header = le32(0)? as usize;
    let w = le32(4)? as i32;
    let h_raw = le32(8)? as i32;
    let bpp = le16(14)? as usize;
    let used = le32(32)? as usize;
    let (h, top_down) = (h_raw.abs(), h_raw < 0);
    if w <= 0 || h <= 0 || w > 4096 || h > 4096 {
        return None;
    }
    let colors = if bpp <= 8 { if used != 0 { used } else { 1 << bpp } } else { 0 };
    let palette = header;
    let bits = header + colors * 4;
    let stride = (w as usize * bpp).div_ceil(32) * 4;
    let mut bm = Bitmap::new(w, h, 0);
    for row in 0..h as usize {
        let src_row = if top_down { row } else { h as usize - 1 - row };
        let line = d.get(bits + src_row * stride..bits + src_row * stride + stride.min(d.len().saturating_sub(bits + src_row * stride)))?;
        for x in 0..w as usize {
            let rgb = match bpp {
                24 => {
                    let p = x * 3;
                    (line[p + 2] as u32) << 16 | (line[p + 1] as u32) << 8 | line[p] as u32
                }
                32 => {
                    let p = x * 4;
                    (line[p + 2] as u32) << 16 | (line[p + 1] as u32) << 8 | line[p] as u32
                }
                8 | 4 | 1 => {
                    let idx = match bpp {
                        8 => line[x] as usize,
                        4 => ((line[x / 2] >> (if x % 2 == 0 { 4 } else { 0 })) & 0xf) as usize,
                        _ => ((line[x / 8] >> (7 - x % 8)) & 1) as usize,
                    };
                    let p = palette + idx * 4;
                    let c = d.get(p..p + 3)?;
                    (c[2] as u32) << 16 | (c[1] as u32) << 8 | c[0] as u32
                }
                _ => return None,
            };
            bm.px[row * w as usize + x] = rgb;
        }
    }
    Some(bm)
}
