//! audioMasterGetTime に返す VstTimeInfo（再生位置、テンポ、拍子）。32 ビットと 64 ビットで同じ並び（double と int32 だけ）
use crate::mem::{Memory, R};

const SAMPLE_POS: u64 = 0;
const SAMPLE_RATE: u64 = 8;
const PPQ_POS: u64 = 24;
const TEMPO: u64 = 32;
const TIME_SIG_NUM: u64 = 64;
const TIME_SIG_DEN: u64 = 68;
const FLAGS: u64 = 84;
const SIZE: usize = 88;
/// 再生中、ppqPos、テンポ、拍子が正しい
const VALID: u64 = 2 | (1 << 9) | (1 << 10) | (1 << 13);
const RATE: f64 = 48000.0;
const BPM: f64 = 120.0;

/// VstTimeInfo を確保して、頭の位置（120 BPM、4/4、再生中）を書く
pub fn alloc(mem: &mut Memory) -> R<u64> {
    let p = mem.alloc32(SIZE) as u64;
    mem.write(p, &[0; SIZE])?;
    mem.write_n(p + SAMPLE_RATE, 8, RATE.to_bits())?;
    mem.write_n(p + TEMPO, 8, BPM.to_bits())?;
    mem.write_n(p + TIME_SIG_NUM, 4, 4)?;
    mem.write_n(p + TIME_SIG_DEN, 4, 4)?;
    mem.write_n(p + FLAGS, 4, VALID)?;
    Ok(p)
}

/// 1 ブロック分（frames サンプル）位置を進める
pub fn advance(mem: &mut Memory, p: u64, frames: usize) -> R<()> {
    let pos = f64::from_bits(mem.read_n(p + SAMPLE_POS, 8)?) + frames as f64;
    mem.write_n(p + SAMPLE_POS, 8, pos.to_bits())?;
    mem.write_n(p + PPQ_POS, 8, (pos / RATE * BPM / 60.0).to_bits())
}
