//! VST3（64 ビットの Windows 版）を読み込む（段階 1: GetPluginFactory からクラスの一覧を取る）。
//! VST3 のオブジェクトは COM に似た形（先頭に関数の表へのポインター。0 queryInterface、1 addRef、2 release のあとに各インターフェースの関数）
use crate::emu::{Arg, Emu};
use crate::mem::{Fault, R};
use crate::pe;

/// クラスの情報（PClassInfo: cid 16 バイト、cardinality、category 32 文字、name 64 文字）
#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub cid: [u8; 16],
    pub category: String,
    pub name: String,
}

pub struct Vst3 {
    pub emu: Emu,
    /// IPluginFactory
    pub factory: u64,
    pub vendor: String,
    pub classes: Vec<ClassInfo>,
}

/// オブジェクトの関数の表の index 番目の関数の番地
pub(crate) fn vfn(emu: &Emu, obj: u64, index: u64) -> R<u64> {
    let table = emu.mem.read_n(obj, 8)?;
    emu.mem.read_n(table + index * 8, 8)
}

/// オブジェクトの関数を呼ぶ（this を先頭に足す）
pub(crate) fn vcall(emu: &mut Emu, obj: u64, index: u64, args: &[Arg]) -> R<u64> {
    let f = vfn(emu, obj, index)?;
    let mut all = vec![Arg::Int(obj)];
    all.extend_from_slice(args);
    emu.call(f, &all)
}

/// 0 終わりの文字列（最大 n バイト）
fn cstr(emu: &Emu, at: u64, n: usize) -> R<String> {
    emu.mem.read_cstr(at, n)
}

/// VST3 の DLL か（GetPluginFactory を書き出し、VST2 の入口 VSTPluginMain を持たない。名前の文字列を探すだけなので、読み込みはしない）
pub fn is_vst3(dll: &[u8]) -> bool {
    let has = |n: &[u8]| dll.windows(n.len()).any(|w| w == n);
    has(b"GetPluginFactory\0") && !has(b"VSTPluginMain\0")
}

impl Vst3 {
    /// DLL のバイト列から読み込み、ファクトリーとクラスの一覧を取る
    pub fn load(dll: &[u8]) -> R<Vst3> {
        // PE の見出しの Machine が 0x8664（x64）か
        let at = dll.get(0x3c..0x40).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize).unwrap_or(0);
        if dll.get(at + 4..at + 6) != Some(&[0x64, 0x86]) {
            return Err(Fault("VST3 は 64 ビットの DLL だけに対応している".into()));
        }
        let mut emu = Emu::new();
        let img = pe::load(&mut emu.mem, dll)?;
        emu.image_base = img.base;
        emu.gui.base = img.base;
        pe::patch_imports(&mut emu, dll, img.base, crate::host_stubs::resolve, crate::host_stubs::unknown)?;
        emu.max_steps = crate::emu::LOAD_MAX_STEPS;
        for cb in pe::setup_tls(&mut emu, dll, img.base)? {
            emu.call(cb, &[Arg::Int(img.base), Arg::Int(1), Arg::Int(0)])?;
        }
        if img.entry != img.base {
            emu.call(img.entry, &[Arg::Int(img.base), Arg::Int(1), Arg::Int(0)])?;
        }
        // Windows の VST3 は InitDll を持つことがある（モジュールの初期化。無くてもよい）
        if let Some(init) = img.export("InitDll") {
            emu.call(init, &[])?;
        }
        let get = img.export("GetPluginFactory").ok_or_else(|| Fault("GetPluginFactory が無い（VST3 ではない）".into()))?;
        let factory = emu.call(get, &[])?;
        if factory == 0 {
            return Err(Fault("GetPluginFactory が 0 を返した".into()));
        }
        // getFactoryInfo(PFactoryInfo*): vendor 64、url 256、email 128、flags
        let info = emu.mem.alloc(512);
        vcall(&mut emu, factory, 3, &[Arg::Int(info)])?;
        let vendor = cstr(&emu, info, 64)?;
        // countClasses()、getClassInfo(index, PClassInfo*)
        let n = vcall(&mut emu, factory, 4, &[])? as u32 as i32;
        let mut classes = Vec::new();
        let ci = emu.mem.alloc(128);
        for i in 0..n.max(0) {
            vcall(&mut emu, factory, 5, &[Arg::Int(i as u64), Arg::Int(ci)])?;
            let mut cid = [0u8; 16];
            emu.mem.read(ci, &mut cid)?;
            classes.push(ClassInfo { cid, category: cstr(&emu, ci + 20, 32)?, name: cstr(&emu, ci + 52, 64)? });
        }
        emu.max_steps = crate::emu::MAX_STEPS;
        Ok(Vst3 { emu, factory, vendor, classes })
    }
}
