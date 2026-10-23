//! host_stubs.rs に無い Windows の API のスタブ（主に、64 ビットの C のランタイムが初期化で呼ぶもの）。スレッドは無く、待つ関数はすぐ戻る
use crate::emu::{Emu, HostFn};
use crate::host_stubs::arg;
use crate::mem::R;

/// 時計の代わり（呼ぶたびに進める。本物の時刻は要らない）
fn tick(e: &mut Emu) -> u64 {
    let t = e.heap_sizes.entry(TICK_KEY).or_insert(0);
    *t += 1000;
    *t as u64
}
/// heap_sizes に、確保した番地と重ならない鍵で値を置く（時計、FLS と TLS の値）
const TICK_KEY: u64 = 1;
const SLOT_KEY: u64 = 0x100;

fn ok(v: u64) -> Option<HostFn> {
    Some(Box::new(move |_e: &mut Emu| -> R<u64> { Ok(v) }))
}

/// 知っている import なら、その振る舞いを返す（dll は大文字）
pub fn resolve(dll: &str, name: &str) -> Option<HostFn> {
    if dll != "KERNEL32.DLL" {
        return match (dll, name) {
            ("WINMM.DLL", "timeGetTime") => Some(Box::new(|e: &mut Emu| -> R<u64> { Ok(tick(e) / 1000 & 0xffff_ffff) })),
            ("OLE32.DLL", "CoInitializeEx" | "OleInitialize") => ok(0),
            // 特別なフォルダー（設定の置き場など）。偽の場所を返す（ファイルは無いので、読み込みは失敗する）
            ("SHELL32.DLL", "SHGetSpecialFolderPathW") => Some(Box::new(|e: &mut Emu| -> R<u64> {
                write_wstr(e, arg(e, 2)?, "C:\\vst")?;
                Ok(1)
            })),
            // 同じく偽の場所を、確保した文字列で返す（4 つ目の引数に番地を書く。解放の CoTaskMemFree は何もしない）
            ("SHELL32.DLL", "SHGetKnownFolderPath") => Some(Box::new(|e: &mut Emu| -> R<u64> {
                let p = e.mem.alloc(16);
                write_wstr(e, p, "C:\\vst")?;
                let w = if e.bits() == 64 { 8 } else { 4 };
                e.mem.write_n(arg(e, 4)?, w, p)?;
                Ok(0)
            })),
            ("OLE32.DLL", "CoTaskMemFree") => ok(0),
            _ => None,
        };
    }
    match name {
        // ポインターの符号化はしない（そのまま返す）。0 を返すと、ランタイムが壊れた関数ポインターとして終了する
        "EncodePointer" | "DecodePointer" | "EncodeSystemPointer" | "DecodeSystemPointer" => Some(Box::new(|e: &mut Emu| -> R<u64> { arg(e, 1) })),
        "InitializeCriticalSectionAndSpinCount" | "InitializeCriticalSectionEx" | "TryEnterCriticalSection" | "HeapSetInformation" | "FlsFree" | "TlsFree" | "CloseHandle" | "SetEvent" | "ResetEvent" | "ReleaseMutex" | "IsValidCodePage" | "IsValidLocale" | "SetThreadPriority" | "FlushFileBuffers" => ok(1),
        "InitializeSListHead" | "InitializeSRWLock" | "AcquireSRWLockExclusive" | "ReleaseSRWLockExclusive" | "AcquireSRWLockShared" | "ReleaseSRWLockShared" | "InitializeConditionVariable" | "WakeConditionVariable" | "WakeAllConditionVariable" | "IsDebuggerPresent" | "SetUnhandledExceptionFilter" | "UnhandledExceptionFilter" | "OutputDebugStringW" | "GetThreadPriority" | "InterlockedFlushSList" => ok(0),
        "GetCurrentProcessId" => ok(0x1000),
        "GetCurrentThread" => ok(u64::MAX - 1),
        "GetUserDefaultLCID" => ok(0x409),
        // 待つものはすぐ戻る（WAIT_OBJECT_0）。イベントなどは偽のハンドル
        "WaitForSingleObject" | "WaitForSingleObjectEx" | "WaitForMultipleObjects" => ok(0),
        "CreateEventW" | "CreateEventA" | "CreateMutexW" | "CreateMutexA" | "CreateSemaphoreW" => ok(0x200),
        "GetSystemTimeAsFileTime" => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let t = 0x01DC_0000_0000_0000 + tick(e);
            e.mem.write_n(arg(e, 1)?, 8, t)?;
            Ok(0)
        })),
        "QueryPerformanceCounter" => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let t = tick(e);
            e.mem.write_n(arg(e, 1)?, 8, t)?;
            Ok(1)
        })),
        "QueryPerformanceFrequency" => Some(Box::new(|e: &mut Emu| -> R<u64> {
            e.mem.write_n(arg(e, 1)?, 8, 10_000_000)?;
            Ok(1)
        })),
        "GetTickCount64" => Some(Box::new(|e: &mut Emu| -> R<u64> { Ok(tick(e) / 1000) })),
        // FLS と TLS（スレッドは 1 つなので、番号ごとの値を 1 つ持つだけ）。0 番は使わない（失敗の値と紛れる）
        "FlsAlloc" | "TlsAlloc" => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let n = e.heap_sizes.entry(SLOT_KEY).or_insert(0);
            *n += 1;
            Ok(*n as u64)
        })),
        "FlsGetValue" | "TlsGetValue" => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let k = SLOT_KEY + 1 + arg(e, 1)?;
            Ok(e.heap_sizes.get(&k).copied().unwrap_or(0) as u64)
        })),
        "FlsSetValue" | "TlsSetValue" => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let k = SLOT_KEY + 1 + arg(e, 1)?;
            let v = arg(e, 2)? as u32 as usize;
            e.heap_sizes.insert(k, v);
            Ok(1)
        })),
        "HeapSize" => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let p = arg(e, 3)?;
            Ok(e.heap_sizes.get(&p).map(|&s| s as u64).unwrap_or(u64::MAX))
        })),
        "GetModuleHandleExW" | "GetModuleHandleExA" => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let out = arg(e, 3)?;
            if out != 0 {
                let base = e.image_base;
                e.mem.write_n(out, 8, base)?;
            }
            Ok(1)
        })),
        "GetStartupInfoW" => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let p = arg(e, 1)?;
            e.mem.write(p, &[0u8; 104])?;
            e.mem.write_n(p, 4, 104)?;
            Ok(0)
        })),
        // スレッドは動かせないので、作ったことにして偽のハンドルを返す（スレッドの関数は呼ばない。6 つ目の引数にスレッドの ID を書く）
        "CreateThread" => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let id = arg(e, 6)?;
            if id != 0 {
                e.mem.write_n(id, 4, 2)?;
            }
            Ok(0x7000_1000)
        })),
        // SYSTEM_INFO（ページは 4KB、確保の単位は 64KB、CPU は 1 つ）
        "GetSystemInfo" | "GetNativeSystemInfo" => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let p = arg(e, 1)?;
            let (w, arch, ty) = if e.bits() == 64 { (8, 9, 8664) } else { (4, 0, 586) };
            e.mem.write(p, &vec![0u8; 16 + 5 * w])?;
            e.mem.write_n(p, 2, arch)?;
            e.mem.write_n(p + 4, 4, 0x1000)?;
            e.mem.write_n(p + 8, w, 0x1_0000)?;
            e.mem.write_n(p + 8 + w as u64, w, 0x7ffe_ffff)?;
            e.mem.write_n(p + 8 + 2 * w as u64, w, 1)?;
            let q = p + 8 + 3 * w as u64;
            e.mem.write_n(q, 4, 1)?;
            e.mem.write_n(q + 4, 4, ty)?;
            e.mem.write_n(q + 8, 4, 0x1_0000)?;
            e.mem.write_n(q + 12, 2, 6)?;
            Ok(0)
        })),
        // ファイルは 1 つも無いことにする（INVALID_FILE_ATTRIBUTES、INVALID_HANDLE_VALUE）
        "GetFileAttributesW" | "GetFileAttributesA" => ok(0xffff_ffff),
        "CreateFileW" | "CreateFileA" | "FindFirstFileW" | "FindFirstFileA" | "FindFirstFileExW" => ok(u64::MAX),
        "GetFileAttributesExW" | "FindNextFileW" | "FindClose" | "GetLocaleInfoW" | "GetLocaleInfoEx" | "GetLocaleInfoA" => ok(0),
        "GetCurrentDirectoryW" => Some(Box::new(|e: &mut Emu| -> R<u64> {
            let (len, buf) = (arg(e, 1)? as u32, arg(e, 2)?);
            if len < 4 || buf == 0 {
                return Ok(4);
            }
            write_wstr(e, buf, "C:\\")?;
            Ok(3)
        })),
        "GetCommandLineW" => Some(Box::new(|e: &mut Emu| -> R<u64> { Ok(e.mem.alloc32(16) as u64) })),
        "GetModuleFileNameW" => Some(Box::new(|e: &mut Emu| -> R<u64> {
            // GetModuleFileNameW(h, buf, size)。偽の場所を返す
            let buf = arg(e, 2)?;
            let path: Vec<u8> = "C:\\plugin.dll\0".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
            e.mem.write(buf, &path)?;
            Ok(13)
        })),
        _ => None,
    }
}

/// ワイド文字の文字列を 0 で終えて書く
fn write_wstr(e: &mut Emu, p: u64, s: &str) -> R<()> {
    let b: Vec<u8> = s.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect();
    e.mem.write(p, &b)
}
