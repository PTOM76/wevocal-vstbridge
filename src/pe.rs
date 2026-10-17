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
    let _ = data;
    Ok(Image { base, size, entry: base + opt.address_of_entry_point() as u64, exports, imports })
}

/// import を解決して IAT（関数へのポインターの表）に thunk の番地を書く（置いたあとのメモリからたどる）。
/// 64 ビットの呼び出しの決まりでは呼ばれた側は引数を片付けないので、thunk は片付けなし
pub fn patch_imports(emu: &mut crate::emu::Emu, data: &[u8], base: u64, resolve: impl Fn(&str, &str) -> Option<crate::emu::HostFn>, mut unknown: impl FnMut(&str, &str) -> crate::emu::HostFn) -> R<()> {
    let file = PeFile64::parse(data).map_err(err)?;
    let Some(dir) = file.data_directories().get(1) else { return Ok(()) };
    let dir_rva = dir.virtual_address.get(LE) as u64;
    if dir_rva == 0 {
        return Ok(());
    }
    // IMAGE_IMPORT_DESCRIPTOR は 20 バイトで、0 で終わる配列
    for i in 0.. {
        let d = base + dir_rva + i * 20;
        let orig = emu.mem.read_n(d, 4)?;
        let name_rva = emu.mem.read_n(d + 12, 4)?;
        let first = emu.mem.read_n(d + 16, 4)?;
        if orig == 0 && name_rva == 0 && first == 0 {
            break;
        }
        let dll = emu.mem.read_cstr(base + name_rva, 260)?;
        let names = if orig != 0 { orig } else { first };
        for j in 0.. {
            let entry = emu.mem.read_n(base + names + j * 8, 8)?;
            if entry == 0 {
                break;
            }
            let name = if entry >> 63 != 0 { format!("#{}", entry & 0xffff) } else { emu.mem.read_cstr(base + (entry & 0x7fff_ffff) + 2, 256)? };
            let f = resolve(&dll, &name).unwrap_or_else(|| unknown(&dll, &name));
            if std::env::var("VSTBRIDGE_IAT").is_ok() {
                eprintln!("[iat] {:#x} {dll} ! {name}", base + first + j * 8);
            }
            // VSTBRIDGE_CALLS なら、知っている関数も呼ばれた順に出す
            let f: crate::emu::HostFn = if std::env::var("VSTBRIDGE_CALLS").is_ok() {
                let label = format!("{dll} ! {name}");
                Box::new(move |e: &mut crate::emu::Emu| {
                    eprintln!("[call64] {label} rcx={:#x} rdx={:#x} r8={:#x} r9={:#x}", e.cpu.regs[1], e.cpu.regs[2], e.cpu.regs[8], e.cpu.regs[9]);
                    f(e)
                })
            } else {
                f
            };
            let at = emu.add_thunk(f);
            emu.mem.write_n(base + first + j * 8, 8, at)?;
        }
    }
    Ok(())
}

/// スレッドごとの変数（TLS）の領域を作る。テンプレートを写した領域を 1 つ作り、TEB の gs:[0x58] の表の 0 番に置き、DLL の _tls_index に 0 を書く。
/// TLS のコールバックの番地を返す（DllMain の前に呼ぶ）
pub fn setup_tls(emu: &mut crate::emu::Emu, data: &[u8], base: u64) -> R<Vec<u64>> {
    let file = PeFile64::parse(data).map_err(err)?;
    let Some(dir) = file.data_directories().get(9) else { return Ok(Vec::new()) };
    let rva = dir.virtual_address.get(LE) as u64;
    if rva == 0 {
        return Ok(Vec::new());
    }
    let d = base + rva;
    let (start, end, index, callbacks) = (emu.mem.read_n(d, 8)?, emu.mem.read_n(d + 8, 8)?, emu.mem.read_n(d + 16, 8)?, emu.mem.read_n(d + 24, 8)?);
    let zero = emu.mem.read_n(d + 32, 4)?;
    let len = end.saturating_sub(start) as usize;
    let block = emu.mem.alloc(len + zero as usize + 16);
    let mut tmpl = vec![0u8; len];
    emu.mem.read(start, &mut tmpl)?;
    emu.mem.write(block, &tmpl)?;
    let table = emu.mem.alloc(64);
    emu.mem.write_n(table, 8, block)?;
    emu.mem.write_n(emu.cpu.fs_base + 0x58, 8, table)?;
    if index != 0 {
        emu.mem.write_n(index, 4, 0)?;
    }
    let mut list = Vec::new();
    if callbacks != 0 {
        for k in 0..64 {
            let f = emu.mem.read_n(callbacks + k * 8, 8)?;
            if f == 0 {
                break;
            }
            list.push(f);
        }
    }
    Ok(list)
}
