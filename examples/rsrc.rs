//! DLL のビットマップのリソースの大きさを表示する（調査用）
use vstbridge::vst32::Vst32;
use vstbridge::rsrc::{find_bitmap, decode_dib, ResName};
fn main() {
    let data = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let v = Vst32::load(&data).unwrap();
    for id in 1..400u32 {
        if let Some(d) = find_bitmap(&v.emu.mem, v.base as u64, &ResName::Id(id)).unwrap() {
            let bpp = u16::from_le_bytes([d[14], d[15]]);
            let (w, h) = (i32::from_le_bytes(d[4..8].try_into().unwrap()), i32::from_le_bytes(d[8..12].try_into().unwrap()));
            println!("id {id}: {w} x {h}, {bpp} bpp, decode={}", decode_dib(&d).is_some());
        }
    }
}
