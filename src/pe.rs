//! Windows の DLL（PE、64 ビット）を読み込み、セクションをメモリに置く。書き出し（export）の番地を引ける
use crate::mem::{Fault, Memory, R};
use object::read::pe::{ImageNtHeaders, ImageOptionalHeader, PeFile64};
use object::{LittleEndian as LE, Object};

pub struct Image {
    pub base: u64,
    pub size: u64,
    pub entry: u64,
    /// 書き出した関数の名前と番地
    pub exports: Vec<(String, u64)>,
    /// 読み込む関数（DLL の名前、関数の名前、IAT の番地）。今は数えるだけで、つなぐのは後
    pub imports: Vec<(String, String, u64)>,
}

impl Image {
    pub fn export(&self, name: &str) -> Option<u64> {
        self.exports.iter().find(|(n, _)| n == name).map(|(_, a)| *a)
    }
}

fn err<E: std::fmt::Display>(e: E) -> Fault {
    Fault(format!("pe: {e}"))
}

/// DLL を希望の番地（ImageBase）に置く（メモリは自由に割り当てられるので、置き直しの補正は要らない）
pub fn load(mem: &mut Memory, data: &[u8]) -> R<Image> {
    let file = PeFile64::parse(data).map_err(err)?;
    let opt = file.nt_headers().optional_header();
    let base = opt.image_base();
    let size = opt.size_of_image() as u64;
    let headers = opt.size_of_headers() as usize;
    mem.map(base, size as usize);
    mem.write(base, &data[..headers.min(data.len())])?;
    for s in file.section_table().iter() {
        let va = s.virtual_address.get(LE) as u64;
        let raw = s.pointer_to_raw_data.get(LE) as usize;
        let len = (s.size_of_raw_data.get(LE) as usize).min(s.virtual_size.get(LE) as usize);
        if len > 0 && raw + len <= data.len() {
            mem.write(base + va, &data[raw..raw + len])?;
        }
    }
    let exports = file
        .exports()
        .map_err(err)?
        .iter()
        .map(|e| (String::from_utf8_lossy(e.name()).into_owned(), e.address()))
        .collect();
    let imports = file
        .imports()
        .map_err(err)?
        .iter()
        .map(|i| (String::from_utf8_lossy(i.library()).into_owned(), String::from_utf8_lossy(i.name()).into_owned(), 0))
        .collect();
    let _ = file.symbols().count();
    Ok(Image { base, size, entry: base + opt.address_of_entry_point() as u64, exports, imports })
}
