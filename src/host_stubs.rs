//! 32 ビットの DLL が import する Windows の API のうち、音の処理の確かめに要る最小限をスタブで用意する。
//! Windows の API はほとんど stdcall（呼ばれた側が引数を片付ける）なので、API ごとの引数の数を表で持つ。
//! 用意していない関数は unknown() の既定の動き（0 を返すだけ。本物の処理はしない）に回す
use crate::emu::{Emu, HostFn};
use crate::mem::R;

/// スタックから n 番目の引数（1 始まり）を読む。ホストの関数が呼ばれる時点では、戻り先はもう pop 済みで、
/// esp は 1 番目の引数を指している（emu.rs の run() の決まり）
fn arg(e: &Emu, n: u64) -> R<u64> {
    let esp = e.cpu.regs[crate::cpu::RSP];
    e.mem.read_n(esp + (n - 1) * 4, 4)
}

/// stdcall の API の引数の数。載っていないものは 0（片付けなし）。Windows の公開されている関数の仕様どおり
pub fn arg_count(name: &str) -> u64 {
    match name {
        "TlsAlloc" | "GetOEMCP" | "GetACP" | "GetCurrentThreadId" | "GetCurrentProcess" | "GetLastError" | "GetVersion" | "GetCommandLineA" | "GetTickCount" | "GetEnvironmentStrings" | "GetEnvironmentStringsW" => 0,
        "LoadLibraryA" | "GetModuleHandleA" | "FreeLibrary" | "TlsGetValue" | "TlsFree" | "Sleep" | "EnterCriticalSection" | "LeaveCriticalSection" | "DeleteCriticalSection" | "InitializeCriticalSection" | "InterlockedIncrement"
        | "InterlockedDecrement" | "SetLastError" | "GetStdHandle" | "SetHandleCount" | "SetUnhandledExceptionFilter" | "GetVersionExA" | "GetStartupInfoA" | "HeapDestroy" | "CloseHandle" | "FlushFileBuffers" | "GetFileType"
        | "ExitProcess" | "FreeEnvironmentStringsA" | "FreeEnvironmentStringsW" => 1,
        "GetProcAddress" | "TlsSetValue" | "TerminateProcess" | "GetCPInfo" | "SetStdHandle" => 2,
        "HeapAlloc" | "HeapFree" | "IsBadWritePtr" | "IsBadReadPtr" | "GetEnvironmentVariableA" | "GetModuleFileNameA" | "VirtualFree" | "HeapCreate" => 3,
        "VirtualAlloc" | "RaiseException" | "SetFilePointer" | "HeapReAlloc" | "RtlUnwind" => 4,
        "WriteFile" | "GetStringTypeW" | "GetStringTypeA" => 5,
        "LCMapStringA" | "LCMapStringW" | "MultiByteToWideChar" => 6,
        "WideCharToMultiByte" => 8,
        _ => 0,
    }
}

/// 知っている import なら、その振る舞いのホスト関数を返す（dll の大文字小文字は気にしない）
pub fn resolve(dll: &str, name: &str) -> Option<HostFn> {
    let dll = dll.to_ascii_uppercase();
    match (dll.as_str(), name) {
        ("KERNEL32.DLL", "LoadLibraryA") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // LoadLibraryA(name)。本物の読み込みはせず、名前を覚えて偽のハンドルを返す（GetProcAddress で名前に戻す）
            let name = e.mem.read_cstr(arg(e, 1)?, 260)?.to_ascii_uppercase();
            let idx = match e.loaded.iter().position(|n| *n == name) {
                Some(i) => i,
                None => {
                    e.loaded.push(name);
                    e.loaded.len() - 1
                }
            };
            Ok(0x7000_0000 + idx as u64 * 0x1_0000)
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
        ("KERNEL32.DLL", "GetModuleHandleA") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0x1000_0000) })),
        ("KERNEL32.DLL", "GetTickCount") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        ("KERNEL32.DLL", "Sleep") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        ("KERNEL32.DLL", "HeapAlloc") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // HeapAlloc(hHeap, flags, size)。要る分だけ確保して返す
            let size = arg(e, 3)?;
            Ok(e.mem.alloc32(size as usize) as u64)
        })),
        ("KERNEL32.DLL", "HeapFree") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(1) })),
        ("KERNEL32.DLL", "VirtualAlloc") => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // VirtualAlloc(addr, size, ...)
            let size = arg(e, 2)?;
            Ok(e.mem.alloc32(size.max(4096) as usize) as u64)
        })),
        ("KERNEL32.DLL", "InitializeCriticalSection" | "EnterCriticalSection" | "LeaveCriticalSection" | "DeleteCriticalSection") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        ("KERNEL32.DLL", "TlsAlloc") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        ("KERNEL32.DLL", "TlsGetValue") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })),
        ("KERNEL32.DLL", "TlsSetValue") => Some(Box::new(|_e: &mut Emu| -> R<u64> { Ok(1) })),
        _ => None,
    }
}

/// 用意していない import（画面を描く GDI32、USER32 など）は、何もせず無害な値を返すだけにする。
/// 本物の挙動とは違うので、画面を描く結果やファイルの読み書きは当てにできない（音の処理だけを試すための割り切り）
pub fn unknown(dll: &str, name: &str) -> HostFn {
    eprintln!("[host_stubs] 未対応の import: {dll} ! {name}（0 を返す）");
    Box::new(|_e: &mut Emu| -> R<u64> { Ok(0) })
}
