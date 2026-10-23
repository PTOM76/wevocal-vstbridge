//! 32 ビットの DLL が import する Windows の API のうち、音の処理の確かめに要る最小限をスタブで用意する。
//! Windows の API はほとんど stdcall（呼ばれた側が引数を片付ける）なので、API ごとの引数の数を表で持つ。
//! 用意していない関数は unknown() の既定の動き（0 を返すだけ。本物の処理はしない）に回す
use crate::emu::{Emu, HostFn};
use crate::mem::R;

/// n 番目の引数（1 始まり）を読む。ホストの関数が呼ばれる時点では、戻り先はもう pop 済み（emu.rs の try_thunk）。
/// 32 ビットは stdcall で、esp が 1 番目の引数を指す。64 ビットは rcx、rdx、r8、r9 のあと、シャドウ領域（32 バイト）の後ろのスタック
pub(crate) fn arg(e: &Emu, n: u64) -> R<u64> {
    let sp = e.cpu.regs[crate::cpu::RSP];
    if e.bits == 64 {
        return match n {
            1 => Ok(e.cpu.regs[1]),
            2 => Ok(e.cpu.regs[2]),
            3 => Ok(e.cpu.regs[8]),
            4 => Ok(e.cpu.regs[9]),
            _ => e.mem.read_n(sp + 0x20 + (n - 5) * 8, 8),
        };
    }
    e.mem.read_n(sp + (n - 1) * 4, 4)
}

/// stdcall の API の引数の数。載っていないものは 0（片付けなし）。Windows の公開されている関数の仕様どおり
pub fn arg_count(name: &str) -> u64 {
    match name {
        "TlsAlloc" | "GetOEMCP" | "GetACP" | "GetCurrentThreadId" | "GetCurrentProcess" | "GetLastError" | "GetVersion" | "GetCommandLineA" | "GetTickCount" | "GetEnvironmentStrings" | "GetEnvironmentStringsW" => 0,
        "LoadLibraryA" | "GetModuleHandleA" | "FreeLibrary" | "TlsGetValue" | "TlsFree" | "Sleep" | "EnterCriticalSection" | "LeaveCriticalSection" | "DeleteCriticalSection" | "InitializeCriticalSection" | "InterlockedIncrement"
        | "InterlockedDecrement" | "SetLastError" | "GetStdHandle" | "SetHandleCount" | "SetUnhandledExceptionFilter" | "GetVersionExA" | "GetStartupInfoA" | "HeapDestroy" | "CloseHandle" | "FlushFileBuffers" | "GetFileType"
        | "ExitProcess" | "FreeEnvironmentStringsA" | "FreeEnvironmentStringsW" | "IsProcessorFeaturePresent" | "GetModuleHandleW" | "SetErrorMode" | "lstrlenA" | "OutputDebugStringA" | "GetSystemInfo" | "QueryPerformanceCounter" | "QueryPerformanceFrequency" | "GetSystemTimeAsFileTime" | "GetTickCount64" | "DisableThreadLibraryCalls" => 1,
        "GetProcAddress" | "TlsSetValue" | "TerminateProcess" | "GetCPInfo" | "SetStdHandle" => 2,
        "HeapAlloc" | "HeapFree" | "IsBadWritePtr" | "IsBadReadPtr" | "GetEnvironmentVariableA" | "GetModuleFileNameA" | "VirtualFree" | "HeapCreate" => 3,
        "VirtualAlloc" | "RaiseException" | "SetFilePointer" | "HeapReAlloc" | "RtlUnwind" => 4,
        "WriteFile" | "GetStringTypeA" => 5,
        "GetStringTypeW" => 4,
        "LCMapStringA" | "LCMapStringW" | "MultiByteToWideChar" => 6,
        "WideCharToMultiByte" => 8,
        // KERNEL32 の足りないもの
        "GlobalFree" | "GlobalLock" | "GlobalUnlock" | "GlobalSize" | "FreeLibrary" | "TlsFree" | "SetUnhandledExceptionFilter" => 1,
        "VirtualFree" => 3,
        _ => crate::win32_args::gui_arg_count(name).unwrap_or(0),
    }
}

/// 知っている import なら、その振る舞いのホスト関数を返す（dll の大文字小文字は気にしない）
pub fn resolve(dll: &str, name: &str) -> Option<HostFn> {
    let mut dll = dll.to_ascii_uppercase();
    // API セット（api-ms-win-core-…）と KERNELBASE の関数は KERNEL32 と同じものとして扱う（新しい C のランタイムは GetProcAddress でこちらから引く）
    if dll.starts_with("API-MS-WIN-") || dll.starts_with("KERNELBASE") {
        dll = "KERNEL32.DLL".into();
    }
    match (dll.as_str(), name) {
        // LoadLibrary(name)。本物の読み込みはせず、名前を覚えて偽のハンドルを返す（GetProcAddress で名前に戻す）
        ("KERNEL32.DLL", "LoadLibraryA" | "LoadLibraryExA") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let name = e.mem.read_cstr(arg(e, 1)?, 260)?;
            Ok(fake_module(e, &name))
        })),
        ("KERNEL32.DLL", "LoadLibraryW" | "LoadLibraryExW") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let name = read_wstr(e, arg(e, 1)?)?;
            Ok(fake_module(e, &name))
        })),
        ("KERNEL32.DLL", "GetProcAddress") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // GetProcAddress(handle, name)。ハンドルから DLL 名を引き、関数名があれば入口を作って返す
            let handle = arg(e, 1)?;
            let name_ptr = arg(e, 2)?;
            // 序数での指定（上位が 0）は扱わない
            if name_ptr < 0x1_0000 {
                return Ok(0);
            }
            let name = e.mem.read_cstr(name_ptr, 128)?;
            let dll = if handle >= 0x7000_0000 { e.loaded.get(((handle - 0x7000_0000) / 0x1_0000) as usize).cloned() } else { Some("KERNEL32.DLL".to_string()) };
            let Some(dll) = dll else { return Ok(0) };
            // 同じ関数には同じ入口を返す
            if let Some(&at) = e.procs.get(&(dll.clone(), name.clone())) {
                return Ok(at);
            }
            let f = resolve(&dll, &name).unwrap_or_else(|| unknown(&dll, &name));
            let at = e.add_thunk_stdcall(f, arg_count(&name));
            e.procs.insert((dll, name), at);
            Ok(at)
        })),
        // ---- C ランタイムの初期化が呼ぶもの（環境、コマンドライン、標準ハンドル、コードページ）。XP の 32 ビット環境を装う ----
        ("KERNEL32.DLL", "GetVersionExA") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // OSVERSIONINFOA: [0]=大きさ、[4]=major、[8]=minor、[12]=build、[16]=platform（2=NT）
            let p = arg(e, 1)?;
            e.mem.write_n(p + 4, 4, 5)?;
            e.mem.write_n(p + 8, 4, 1)?;
            e.mem.write_n(p + 12, 4, 2600)?;
            e.mem.write_n(p + 16, 4, 2)?;
            Ok(1)
        })),
        ("KERNEL32.DLL", "GetCommandLineA") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let at = e.mem.alloc32(16);
            e.mem.write(at as u64, b"plugin.dll\0")?;
            Ok(at as u64)
        })),
        ("KERNEL32.DLL", "GetEnvironmentStringsW") => Some(Box::new(|e: &mut Emu| -> R<u64> { Ok(e.mem.alloc32(16) as u64) })),
        ("KERNEL32.DLL", "GetEnvironmentStrings") => Some(Box::new(|e: &mut Emu| -> R<u64> { Ok(e.mem.alloc32(16) as u64) })),
        ("KERNEL32.DLL", "FreeEnvironmentStringsA" | "FreeEnvironmentStringsW") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(1) })),
        ("KERNEL32.DLL", "GetEnvironmentVariableA") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        ("KERNEL32.DLL", "GetStartupInfoA") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // STARTUPINFOA（68 バイト）を 0 で埋め、cb だけ書く
            let p = arg(e, 1)?;
            e.mem.write(p, &[0u8; 68])?;
            e.mem.write_n(p, 4, 68)?;
            Ok(0)
        })),
        ("KERNEL32.DLL", "GetStdHandle") => Some(Box::new(|e: &mut Emu| -> R<u64> { Ok(0x100 + (arg(e, 1)? & 0xf)) })),
        ("KERNEL32.DLL", "GetFileType") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(2) })),
        ("KERNEL32.DLL", "SetHandleCount") => Some(Box::new(|e: &mut Emu| -> R<u64> { arg(e, 1) })),
        ("KERNEL32.DLL", "GetACP" | "GetOEMCP") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(1252) })),
        ("KERNEL32.DLL", "GetModuleFileNameA") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // GetModuleFileNameA(module, buf, size)
            let buf = arg(e, 2)?;
            let name = b"C:\\plugin.dll\0";
            e.mem.write(buf, name)?;
            Ok(name.len() as u64 - 1)
        })),
        ("KERNEL32.DLL", "GetCurrentThreadId") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(1) })),
        ("KERNEL32.DLL", "GetCurrentProcess") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0xffff_ffff) })),
        ("KERNEL32.DLL", "GetCPInfo") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // CPINFO: [0]=MaxCharSize（1）、DefaultChar[2]、LeadByte[12]
            let p = arg(e, 2)?;
            e.mem.write(p, &[0u8; 20])?;
            e.mem.write_n(p, 4, 1)?;
            Ok(1)
        })),
        ("KERNEL32.DLL", "GetLastError") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        ("KERNEL32.DLL", "SetLastError") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        ("KERNEL32.DLL", "InterlockedIncrement") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let p = arg(e, 1)?;
            let v = (e.mem.read_n(p, 4)? as u32).wrapping_add(1);
            e.mem.write_n(p, 4, v as u64)?;
            Ok(v as u64)
        })),
        ("KERNEL32.DLL", "InterlockedDecrement") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let p = arg(e, 1)?;
            let v = (e.mem.read_n(p, 4)? as u32).wrapping_sub(1);
            e.mem.write_n(p, 4, v as u64)?;
            Ok(v as u64)
        })),
        ("KERNEL32.DLL", "IsBadReadPtr" | "IsBadWritePtr" | "IsBadCodePtr") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        // 文字の変換。ランタイムの初期化で呼ばれる程度なので、1 バイト文字（ASCII）として写すだけ
        ("KERNEL32.DLL", "MultiByteToWideChar") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // (cp, flags, src, srclen, dst, dstlen)。srclen が -1 なら 0 終端まで（0 を含む）
            let (src, srclen, dst, dstlen) = (arg(e, 3)?, arg(e, 4)? as i32, arg(e, 5)?, arg(e, 6)? as u32 as usize);
            let n = if srclen < 0 { e.mem.read_cstr(src, 4096)?.len() + 1 } else { srclen as usize };
            if dstlen == 0 || dst == 0 {
                return Ok(n as u64);
            }
            for i in 0..n.min(dstlen) {
                let c = e.mem.read_n(src + i as u64, 1)?;
                e.mem.write_n(dst + i as u64 * 2, 2, c)?;
            }
            Ok(n.min(dstlen) as u64)
        })),
        ("KERNEL32.DLL", "WideCharToMultiByte") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // (cp, flags, src, srclen, dst, dstlen, defchar, usedefchar)
            let (src, srclen, dst, dstlen) = (arg(e, 3)?, arg(e, 4)? as i32, arg(e, 5)?, arg(e, 6)? as u32 as usize);
            let mut n = 0usize;
            if srclen < 0 {
                while e.mem.read_n(src + n as u64 * 2, 2)? != 0 {
                    n += 1;
                }
                n += 1;
            } else {
                n = srclen as usize;
            }
            if dstlen == 0 || dst == 0 {
                return Ok(n as u64);
            }
            for i in 0..n.min(dstlen) {
                let c = e.mem.read_n(src + i as u64 * 2, 2)?;
                e.mem.write_n(dst + i as u64, 1, if c < 0x80 { c } else { 0x3f })?;
            }
            Ok(n.min(dstlen) as u64)
        })),
        // 文字の種類（CT_CTYPE1）。ASCII の範囲だけ埋める。A は (locale, type, src, count, out)、W は (type, src, count, out) でワイド文字
        ("KERNEL32.DLL", "GetStringTypeA") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let (src, count, out) = (arg(e, 3)?, arg(e, 4)? as i32, arg(e, 5)?);
            let n = if count < 0 { e.mem.read_cstr(src, 4096)?.len() + 1 } else { count as usize };
            for i in 0..n {
                let c = e.mem.read_n(src + i as u64, 1)? as u8;
                e.mem.write_n(out + i as u64 * 2, 2, ctype1(c))?;
            }
            Ok(1)
        })),
        ("KERNEL32.DLL", "GetStringTypeW") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let (src, count, out) = (arg(e, 2)?, arg(e, 3)? as i32, arg(e, 4)?);
            let n = if count < 0 {
                let mut k = 0u64;
                while e.mem.read_n(src + k * 2, 2)? != 0 {
                    k += 1;
                }
                k as usize + 1
            } else {
                count as usize
            };
            for i in 0..n {
                let c = e.mem.read_n(src + i as u64 * 2, 2)?;
                e.mem.write_n(out + i as u64 * 2, 2, if c < 0x80 { ctype1(c as u8) } else { 0 })?;
            }
            Ok(1)
        })),
        ("KERNEL32.DLL", "LCMapStringA") => Some(Box::new(|e: &mut Emu| -> R<u64> { lc_map(e, 1) })),
        ("KERNEL32.DLL", "LCMapStringW") => Some(Box::new(|e: &mut Emu| -> R<u64> { lc_map(e, 2) })),
        // CPU の機能（SSE2 など）は全部「無い」と答え、最小の命令だけで動くようにする
        ("KERNEL32.DLL", "IsProcessorFeaturePresent") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        ("KERNEL32.DLL", "GetModuleHandleA") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let p = arg(e, 1)?;
            let name = if p == 0 { String::new() } else { e.mem.read_cstr(p, 260)? };
            Ok(module_handle(e, p, &name))
        })),
        ("KERNEL32.DLL", "GetModuleHandleW") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let p = arg(e, 1)?;
            let name = if p == 0 { String::new() } else { read_wstr(e, p)? };
            Ok(module_handle(e, p, &name))
        })),
        ("KERNEL32.DLL", "GetTickCount") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        ("KERNEL32.DLL", "Sleep") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        // ヒープ。C のランタイムは、ヒープを作れないとその後のメモリ確保が全部失敗する（リバーブのバッファなどが空になる）
        ("KERNEL32.DLL", "HeapCreate") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0x7100_0000) })),
        ("KERNEL32.DLL", "GetProcessHeap") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0x7100_0000) })),
        ("KERNEL32.DLL", "HeapDestroy") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(1) })),
        // Windows XP（5.1）を名乗る。下位 16 ビットが版（上位バイトが minor、下位バイトが major）、上位 16 ビットがビルド番号
        ("KERNEL32.DLL", "GetVersion") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0x0A28_0105) })),
        ("KERNEL32.DLL", "HeapAlloc") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // HeapAlloc(hHeap, flags, size)。要る分だけ確保して返す（HEAP_ZERO_MEMORY は、新しい領域が 0 なのでそのまま）
            let size = arg(e, 3)? as u32 as usize;
            let at = e.mem.alloc32(size);
            e.heap_sizes.insert(at as u64, size);
            if std::env::var("VSTBRIDGE_HEAP").is_ok() {
                eprintln!("[heap] HeapAlloc {size} バイト → {at:#x}");
            }
            Ok(at as u64)
        })),
        ("KERNEL32.DLL", "HeapReAlloc") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // HeapReAlloc(hHeap, flags, ptr, size)。新しく確保して、古い中身を写す
            let old = arg(e, 3)?;
            let size = arg(e, 4)? as u32 as usize;
            let at = e.mem.alloc32(size);
            let n = e.heap_sizes.get(&old).copied().unwrap_or(0).min(size);
            if n > 0 {
                let mut buf = vec![0u8; n];
                e.mem.read(old, &mut buf)?;
                e.mem.write(at as u64, &buf)?;
            }
            e.heap_sizes.insert(at as u64, size);
            Ok(at as u64)
        })),
        ("KERNEL32.DLL", "HeapFree") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // HeapFree(hHeap, flags, ptr)。確保していないポインター、二重の解放を報告する（本物の Windows はヒープの破壊として終了する。調査用 VSTBRIDGE_HEAP=1）
            let ptr = arg(e, 3)?;
            if ptr != 0 {
                let known = e.heap_sizes.remove(&ptr).is_some();
                if !known && std::env::var("VSTBRIDGE_HEAP").is_ok() {
                    eprintln!("[heap] HeapFree: 確保していない、または解放済みのポインター {ptr:#x}");
                }
            }
            Ok(1)
        })),
        ("KERNEL32.DLL", "VirtualAlloc") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // VirtualAlloc(addr, size, ...)
            let size = arg(e, 2)?;
            Ok(e.mem.alloc32(size.max(4096) as usize) as u64)
        })),
        ("KERNEL32.DLL", "InitializeCriticalSection" | "EnterCriticalSection" | "LeaveCriticalSection" | "DeleteCriticalSection") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        ("KERNEL32.DLL", "TlsAlloc") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        ("KERNEL32.DLL", "TlsGetValue") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        ("KERNEL32.DLL", "TlsSetValue") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(1) })),
        // 画面（エディター）の API は gui_api.rs（MSIMG32 の TransparentBlt、AlphaBlend も）
        ("USER32.DLL" | "GDI32.DLL" | "MSIMG32.DLL", name) => crate::gui_api::resolve(name),
        (dll, name) => crate::win_stubs::resolve(dll, name),
    }
}

/// 用意していない import（画面を描く GDI32、USER32 など）は、何もせず無害な値を返すだけにする。
/// 本物の挙動とは違うので、画面を描く結果やファイルの読み書きは当てにできない（音の処理だけを試すための割り切り）
pub fn unknown(dll: &str, name: &str) -> HostFn {
    eprintln!("[host_stubs] 未対応の import: {dll} ! {name}（0 を返す）");
    let label = format!("{dll} ! {name}");
    Box::new(move |_e: &mut Emu| -> R<u64> {
        // 実際に呼ばれたときだけ出す（VSTBRIDGE_CALLS=1）。何が足りないかを調べるため
        if std::env::var("VSTBRIDGE_CALLS").is_ok() {
            eprintln!("[call] {label}");
        }
        Ok(0)
    })
}

/** ASCII 1 文字の CT_CTYPE1 の種類（C1_UPPER=1、C1_LOWER=2、C1_DIGIT=4、C1_SPACE=8、C1_PUNCT=0x10、C1_CNTRL=0x20、C1_BLANK=0x40、C1_XDIGIT=0x80、C1_ALPHA=0x100） */
fn ctype1(c: u8) -> u64 {
    let ch = c as char;
    (ch.is_ascii_uppercase() as u64)
        | (ch.is_ascii_lowercase() as u64) << 1
        | (ch.is_ascii_digit() as u64) << 2
        | (ch.is_ascii_whitespace() as u64) << 3
        | (ch.is_ascii_punctuation() as u64) << 4
        | (ch.is_ascii_control() as u64) << 5
        | ((c == b' ' || c == 9) as u64) << 6
        | (ch.is_ascii_hexdigit() as u64) << 7
        | (ch.is_ascii_alphabetic() as u64) << 8
}

/// LCMapString(locale, flags, src, srclen, dst, dstlen)。変換はせず写すだけ（dstlen が 0 なら必要な長さ）。unit は 1 文字のバイト数（A は 1、W は 2）
fn lc_map(e: &mut Emu, unit: usize) -> R<u64> {
    let (src, srclen, dst, dstlen) = (arg(e, 3)?, arg(e, 4)? as i32, arg(e, 5)?, arg(e, 6)? as u32 as usize);
    if src == 0 {
        return Ok(0);
    }
    let n = if srclen >= 0 {
        srclen as usize
    } else {
        // 0 で終わるまで（終わりの 0 も含める）
        let mut k = 0;
        while e.mem.read_n(src + (k * unit) as u64, unit)? != 0 {
            k += 1;
        }
        k + 1
    };
    if dstlen == 0 || dst == 0 {
        return Ok(n as u64);
    }
    let m = n.min(dstlen);
    let mut buf = vec![0u8; m * unit];
    e.mem.read(src, &mut buf)?;
    e.mem.write(dst, &buf)?;
    Ok(m as u64)
}

/// ワイド文字の文字列（0 で終わる）を読む
pub(crate) fn read_wstr(e: &Emu, p: u64) -> R<String> {
    let mut out = Vec::new();
    for k in 0..520u64 {
        let c = e.mem.read_n(p + k * 2, 2)? as u16;
        if c == 0 {
            break;
        }
        out.push(c);
    }
    Ok(String::from_utf16_lossy(&out))
}

/// 名前を覚えて、偽のモジュールのハンドルを返す（GetProcAddress で名前に戻す）
fn fake_module(e: &mut Emu, name: &str) -> u64 {
    let name = name.to_ascii_uppercase();
    let idx = match e.loaded.iter().position(|n| *n == name) {
        Some(i) => i,
        None => {
            e.loaded.push(name);
            e.loaded.len() - 1
        }
    };
    0x7000_0000 + idx as u64 * 0x1_0000
}

/// GetModuleHandle。自分（NULL）なら DLL の番地。Windows の基本の DLL（KERNEL32 など）は偽のハンドル、そのほか（mscoree など）は無いことにする
fn module_handle(e: &mut Emu, p: u64, name: &str) -> u64 {
    if p == 0 {
        return if e.image_base == 0 { 0x1000_0000 } else { e.image_base };
    }
    let up = name.to_ascii_uppercase();
    if ["KERNEL32", "KERNELBASE", "NTDLL", "USER32", "API-MS-WIN"].iter().any(|k| up.contains(k)) { fake_module(e, &up) } else if e.image_base == 0 { 0x1000_0000 } else { 0 }
}
