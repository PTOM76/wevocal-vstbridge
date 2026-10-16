//! 32 ビットの PE（DLL）を読み込む。64 ビット版（pe.rs）と違い、import を自分でたどって IAT（関数へのポインターの表）に
//! ホストの thunk の番地を書き込む（呼ばれたら Rust の関数に回すため）。32 ビットの古い VST2 は Windows API への依存が多いため
use crate::emu::Emu;
use crate::mem::{Fault, R};
use object::read::pe::{ImageNtHeaders, ImageOptionalHeader, PeFile32};
use object::{LittleEndian as LE, Object};

pub struct Image32 {
    pub base: u32,
    pub entry: u32,
    pub exports: Vec<(String, u32)>,
}

impl Image32 {
    pub fn export(&self, name: &str) -> Option<u32> {
        self.exports.iter().find(|(n, _)| n == name).map(|(_, a)| *a)
    }
}

fn err<E: std::fmt::Display>(e: E) -> Fault {
    Fault(format!("pe32: {e}"))
}

/// DLL を読み込み、セクションを置く。import は resolve で解決する関数に、見つからなければ unknown に回す
pub fn load(emu: &mut Emu, data: &[u8], resolve: impl Fn(&str, &str) -> Option<crate::emu::HostFn>, mut unknown: impl FnMut(&str, &str) -> crate::emu::HostFn) -> R<Image32> {
    let file = PeFile32::parse(data).map_err(err)?;
    let opt = file.nt_headers().optional_header();
    let base = opt.image_base() as u32;
    let size = opt.size_of_image();
    let headers = opt.size_of_headers() as usize;
    let mem = &mut emu.mem;
    mem.map(base as u64, size as usize);
    mem.write(base as u64, &data[..headers.min(data.len())])?;
    for s in file.section_table().iter() {
        let va = s.virtual_address.get(LE);
        let raw = s.pointer_to_raw_data.get(LE) as usize;
        let len = (s.size_of_raw_data.get(LE) as usize).min(s.virtual_size.get(LE) as usize);
        if len > 0 && raw + len <= data.len() {
            mem.write((base + va) as u64, &data[raw..raw + len])?;
        }
    }
    let exports = file
        .exports()
        .map_err(err)?
        .iter()
        .map(|e| (String::from_utf8_lossy(e.name()).into_owned(), e.address() as u32))
        .collect();

    // import ディレクトリを自分でたどり、IAT（first_thunk の指す配列）に thunk の番地を書く
    let dirs = file.data_directories();
    if let Some(dir) = dirs.get(1) {
        let rva = dir.virtual_address.get(LE);
        let dir_size = dir.size.get(LE);
        if rva != 0 {
            patch_imports(emu, data, base, rva, dir_size, &resolve, &mut unknown)?;
        }
    }
    Ok(Image32 { base, entry: base + opt.address_of_entry_point(), exports })
}

/// RVA（番地の相対値）をファイルの位置に変換する（セクションのテーブルから）
fn rva_to_file(data: &[u8], file: &PeFile32, rva: u32) -> Option<usize> {
    for s in file.section_table().iter() {
        let va = s.virtual_address.get(LE);
        let vsize = s.virtual_size.get(LE);
        if rva >= va && rva < va + vsize {
            let raw = s.pointer_to_raw_data.get(LE) as usize;
            let off = raw + (rva - va) as usize;
            return (off < data.len()).then_some(off);
        }
    }
    None
}

fn cstr(data: &[u8], off: usize) -> String {
    let end = data[off..].iter().position(|&b| b == 0).map(|p| off + p).unwrap_or(data.len());
    String::from_utf8_lossy(&data[off..end]).into_owned()
}

fn patch_imports(
    emu: &mut Emu,
    data: &[u8],
    base: u32,
    dir_rva: u32,
    dir_size: u32,
    resolve: &impl Fn(&str, &str) -> Option<crate::emu::HostFn>,
    unknown: &mut impl FnMut(&str, &str) -> crate::emu::HostFn,
) -> R<()> {
    let file = PeFile32::parse(data).map_err(err)?;
    let mut i = 0u32;
    // IMAGE_IMPORT_DESCRIPTOR は 20 バイト。size_of_image の中、個数は 0 で終わる配列
    loop {
        if (i + 1) * 20 > dir_size.max(20) && dir_size != 0 {
            // dir_size が信用できないビルドもあるので、0 エントリーで止める判定を優先する
        }
        let Some(desc_off) = rva_to_file(data, &file, dir_rva + i * 20) else { break };
        let desc = &data[desc_off..desc_off + 20];
        let orig_first_thunk = u32::from_le_bytes(desc[0..4].try_into().unwrap());
        let name_rva = u32::from_le_bytes(desc[12..16].try_into().unwrap());
        let first_thunk = u32::from_le_bytes(desc[16..20].try_into().unwrap());
        if name_rva == 0 && first_thunk == 0 && orig_first_thunk == 0 {
            break;
        }
        let dll_name = rva_to_file(data, &file, name_rva).map(|o| cstr(data, o)).unwrap_or_default();
        let thunk_rva = if orig_first_thunk != 0 { orig_first_thunk } else { first_thunk };
        let mut j = 0u32;
        loop {
            let Some(toff) = rva_to_file(data, &file, thunk_rva + j * 4) else { break };
            let entry = u32::from_le_bytes(data[toff..toff + 4].try_into().unwrap());
            if entry == 0 {
                break;
            }
            let fn_name = if entry & 0x8000_0000 != 0 {
                format!("#{}", entry & 0xffff)
            } else {
                rva_to_file(data, &file, entry).map(|o| cstr(data, o + 2)).unwrap_or_default()
            };
            let host_fn = resolve(&dll_name, &fn_name).unwrap_or_else(|| unknown(&dll_name, &fn_name));
            let thunk_addr = emu.add_thunk_stdcall(host_fn, crate::host_stubs::arg_count(&fn_name));
            // IAT の実体（first_thunk の配列）に thunk の番地を書く。32 ビットなので下位 32 ビットだけ
            emu.mem.write_n((base + first_thunk + j * 4) as u64, 4, thunk_addr)?;
            j += 1;
        }
        i += 1;
    }
    Ok(())
}
