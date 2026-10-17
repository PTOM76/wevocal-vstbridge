//! 32 ビットの VST2 の DLL を本物の CPU で動かし、エミュレーターと同じ手順（初期化、つまみ、インパルス、128 サンプルずつ）で出力を出す。
//! 使い方: native32.exe <dll> [つまみの番号=値 ...]   例: native32.exe "SH-1 Reverb.dll" 9=0.8 10=0
use std::ffi::{c_void, CString};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

const MAGIC: i32 = 0x56737450;

/// 32 ビットの AEffect（ポインターが 4 バイト）
#[repr(C)]
struct AEffect {
    magic: i32,
    dispatcher: extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize,
    process: usize,
    set_parameter: extern "C" fn(*mut AEffect, i32, f32),
    get_parameter: extern "C" fn(*mut AEffect, i32) -> f32,
    num_programs: i32,
    num_params: i32,
    num_inputs: i32,
    num_outputs: i32,
    flags: i32,
    resvd1: isize,
    resvd2: isize,
    initial_delay: i32,
    real_qualities: i32,
    off_qualities: i32,
    io_ratio: f32,
    object: *mut c_void,
    user: *mut c_void,
    unique_id: i32,
    version: i32,
    process_replacing: extern "C" fn(*mut AEffect, *mut *mut f32, *mut *mut f32, i32),
}

extern "C" fn host(_e: *mut AEffect, opcode: i32, _i: i32, _v: isize, _p: *mut c_void, _o: f32) -> isize {
    eprintln!("[audioMaster] opcode={opcode}");
    if opcode == 1 { std::env::var("VER").ok().and_then(|v| v.parse().ok()).unwrap_or(2400) } else { 0 }
}

unsafe extern "system" fn on_crash(info: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS) -> i32 {
    let r = &*(*info).ExceptionRecord;
    let c = &*(*info).ContextRecord;
    eprintln!("[クラッシュ] 例外コード={:#x} 番地={:?} eip={:#x} eax={:#x} ecx={:#x} edx={:#x} esp={:#x}", r.ExceptionCode, r.ExceptionAddress, c.Eip, c.Eax, c.Ecx, c.Edx, c.Esp);
    1
}

unsafe extern "system" fn on_veh(info: *mut windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS) -> i32 {
    let r = &*(*info).ExceptionRecord;
    let c = &*(*info).ContextRecord;
    // 0x406D1388 はデバッガーへのスレッド名の通知、0xE06D7363 は C++ の例外（これらは普通に起きる）
    if r.ExceptionCode as u32 != 0x406D1388 {
        eprintln!("[例外] コード={:#x} 番地={:?} eip={:#x} eax={:#x} ebx={:#x} ecx={:#x} edx={:#x} esp={:#x}", r.ExceptionCode as u32, r.ExceptionAddress, c.Eip, c.Eax, c.Ebx, c.Ecx, c.Edx, c.Esp);
    }
    // スタックの中身から、プラグインの DLL の中らしい番地（0x10000000 台）を拾い、どこから呼ばれたかの手がかりにする
    if r.ExceptionCode as u32 == 0xc0000374 {
        let sp = c.Esp as *const u32;
        let mut hits = Vec::new();
        for i in 0..400 {
            let v = *sp.add(i);
            if (0x1000_0000..0x1002_0000).contains(&v) {
                hits.push(format!("[esp+{:#x}]={:#x}", i * 4, v));
            }
        }
        eprintln!("[スタックの中身] プラグインの番地らしいもの: {}", hits.join(" "));
    }
    if r.ExceptionCode as u32 == 0xc0000374 && std::env::var("IGNORE_HEAP").is_ok() {
        return -1; // EXCEPTION_CONTINUE_EXECUTION
    }
    0 // EXCEPTION_CONTINUE_SEARCH
}

fn main() {
    unsafe { windows_sys::Win32::System::Diagnostics::Debug::AddVectoredExceptionHandler(1, Some(on_veh)); }
    unsafe { windows_sys::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(on_crash)); }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("DLL のパスを渡す");
    let c = CString::new(path.as_str()).unwrap();
    unsafe {
        let m = LoadLibraryA(c.as_ptr() as *const u8);
        assert!(!m.is_null(), "LoadLibrary に失敗");
        let name = CString::new("VSTPluginMain").unwrap();
        let mut f = GetProcAddress(m, name.as_ptr() as *const u8);
        if f.is_none() {
            let main = CString::new("main").unwrap();
            f = GetProcAddress(m, main.as_ptr() as *const u8);
        }
        let entry: extern "C" fn(extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize) -> *mut AEffect = std::mem::transmute(f.expect("VSTPluginMain が無い"));
        eprintln!("VSTPluginMain を呼ぶ");
        let e = entry(host);
        eprintln!("AEffect = {e:?}");
        assert!(!e.is_null() && (*e).magic == MAGIC, "AEffect が不正");
        let d = |op: i32, idx: i32, val: isize, ptr: *mut c_void, opt: f32| ((*e).dispatcher)(e, op, idx, val, ptr, opt);
        // エミュレーターと同じ初期化（開く、プログラム、サンプルレート、ブロックサイズ、電源オン）
        eprintln!("effOpen");
        d(0, 0, 0, std::ptr::null_mut(), 0.0);
        eprintln!("effSetProgram");
        if std::env::var("NO_PROGRAM").is_err() {
            d(2, 0, 0, std::ptr::null_mut(), 0.0);
        }
        eprintln!("effSetSampleRate");
        d(10, 0, 0, std::ptr::null_mut(), std::env::var("SR").ok().and_then(|v| v.parse().ok()).unwrap_or(48000.0));
        d(11, 0, 2048, std::ptr::null_mut(), 0.0);
        eprintln!("effMainsChanged");
        d(12, 0, 1, std::ptr::null_mut(), 0.0);
        let mut buf = [0u8; 64];
        d(45, 0, 0, buf.as_mut_ptr() as *mut c_void, 0.0);
        let nm = String::from_utf8_lossy(&buf).trim_end_matches('\0').to_string();
        eprintln!("name={nm:?} params={} in={} out={}", (*e).num_params, (*e).num_inputs, (*e).num_outputs);
        for a in &args[1..] {
            let (i, v) = a.split_once('=').expect("番号=値");
            ((*e).set_parameter)(e, i.parse().unwrap(), v.parse().unwrap());
        }
        for i in 0..(*e).num_params {
            eprint!("{}={:.3} ", i, ((*e).get_parameter)(e, i));
        }
        eprintln!();
        let n = 128usize;
        let mut energies = Vec::new();
        for b in 0..200 {
            let mut inl = vec![0f32; n];
            let mut inr = vec![0f32; n];
            if b == 0 {
                inl[0] = 1.0;
                inr[0] = 1.0;
            }
            let mut outl = vec![0f32; n];
            let mut outr = vec![0f32; n];
            let mut ins = [inl.as_mut_ptr(), inr.as_mut_ptr()];
            let mut outs = [outl.as_mut_ptr(), outr.as_mut_ptr()];
            ((*e).process_replacing)(e, ins.as_mut_ptr(), outs.as_mut_ptr(), n as i32);
            if let Some(k) = outl.iter().position(|x| !x.is_finite()) {
                eprintln!("ブロック {b} の {k} サンプル目で NaN");
                break;
            }
            energies.push(outl.iter().map(|x| x.abs()).sum::<f32>());
        }
        let r = |x: f32| (x * 1000.0).round() / 1000.0;
        eprintln!("ブロックごとのエネルギー（先頭 12）: {:?}", energies.iter().take(12).map(|x| r(*x)).collect::<Vec<_>>());
        eprintln!("合計 {:.4}、尾（ブロック 4 以降）= {:.4}、最後のブロック {:.4}", energies.iter().sum::<f32>(), energies.iter().skip(4).sum::<f32>(), energies.last().copied().unwrap_or(0.0));
    }
}
