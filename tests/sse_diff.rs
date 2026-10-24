//! SSE の命令（xmm0, xmm1 の形と、即値がつく形）を 1 つずつ、本物の CPU とエミュレーターで同じ入力から動かして、xmm0 とフラグを比べる。
//! エミュレーターが対応していない命令と、この CPU に無い命令は飛ばす。x86-64 の Windows でだけ動く。
//! そろえられない違いは比べない: rcpps、rsqrtps などの近似（CPU の製品ごとに値が違う。エミュレーターは正確に割る）と、結果が NaN のときの中身
#![cfg(all(target_arch = "x86_64", windows))]
use iced_x86::{Code, Encoder, EncodingKind, Instruction, InstructionInfoFactory, Register, RflagsBits};
use vstbridge::emu::Emu;

unsafe extern "system" {
    fn VirtualAlloc(addr: *mut u8, size: usize, ty: u32, protect: u32) -> *mut u8;
}

/// 入力と出力の置き場（スタブが rcx で受け取る）
#[repr(C)]
#[derive(Default)]
struct Io {
    x0: u128,
    x1: u128,
    out: u128,
    flags: u64,
}

/// 命令 1 つを本物の CPU で動かすスタブ: xmm0、xmm1 を読み、命令、xmm0 とフラグを書いて戻る
fn native(code: &[u8], io: &mut Io) {
    let mut stub = vec![0xF3, 0x0F, 0x6F, 0x01, 0xF3, 0x0F, 0x6F, 0x49, 0x10];
    stub.extend_from_slice(code);
    // movdqu [rcx+32], xmm0; pushfq; pop rax; mov [rcx+48], rax; ret
    stub.extend_from_slice(&[0xF3, 0x0F, 0x7F, 0x41, 0x20, 0x9C, 0x58, 0x48, 0x89, 0x41, 0x30, 0xC3]);
    // SAFETY: 実行できるページを確保してスタブを置き、Win64 の決まりで呼ぶ（xmm0、xmm1、rax は呼ばれた側が壊してよい）
    unsafe {
        let p = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 0x40);
        std::ptr::copy_nonoverlapping(stub.as_ptr(), p, stub.len());
        let f: extern "system" fn(*mut Io) = std::mem::transmute(p);
        f(io);
    }
}

/// 同じ命令をエミュレーターで動かす。対応していなければ None
fn emulated(code: &[u8], x0: u128, x1: u128) -> Option<(u128, u64)> {
    let mut e = Emu::new();
    let at = 0x2000_0000u64;
    e.mem.map(at, 4096);
    let mut bytes = code.to_vec();
    bytes.push(0xC3);
    e.mem.write(at, &bytes).ok()?;
    e.cpu.xmm[0] = x0;
    e.cpu.xmm[1] = x1;
    e.call(at, &[]).ok()?;
    let f = &e.cpu.flags;
    let flags = (f.cf as u64) | (f.pf as u64) << 2 | (f.zf as u64) << 6 | (f.sf as u64) << 7 | (f.of as u64) << 11;
    Some((e.cpu.xmm[0], flags))
}

/// 同じ結果か。NaN の組（32 か 64 ビットとして見て、両方 NaN）は中身が違っても同じとみなす
fn same(a: u128, b: u128) -> bool {
    let nan_eq = |bits: u32| {
        let n = 128 / bits;
        let m = if bits == 32 { 0xffff_ffffu128 } else { u64::MAX as u128 };
        (0..n).all(|k| {
            let (x, y) = ((a >> (k * bits)) & m, (b >> (k * bits)) & m);
            let nan = |v: u128| if bits == 32 { f32::from_bits(v as u32).is_nan() } else { f64::from_bits(v as u64).is_nan() };
            x == y || (nan(x) && nan(y))
        })
    };
    a == b || nan_eq(32) || nan_eq(64)
}

/// この CPU で動く命令か（SSE〜SSE4.2 だけを試す）
fn supported(i: &Instruction) -> bool {
    use iced_x86::CpuidFeature as F;
    let mut f = InstructionInfoFactory::new();
    f.info(i);
    i.cpuid_features().iter().all(|c| match c {
        F::SSE => true,
        F::SSE2 => true,
        F::SSE3 => is_x86_feature_detected!("sse3"),
        F::SSSE3 => is_x86_feature_detected!("ssse3"),
        F::SSE4_1 => is_x86_feature_detected!("sse4.1"),
        F::SSE4_2 => is_x86_feature_detected!("sse4.2"),
        _ => false,
    })
}

/// 決まった列の乱数
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 11 ^ self.0 << 13
    }
    /// いろいろな形の 16 バイト（ばらばらのバイト、f32、f64、小さな整数、特別な値）
    fn xmm(&mut self) -> u128 {
        let pick = self.next() % 6;
        let lanes32 = |r: &mut Rng, f: &dyn Fn(&mut Rng) -> u32| (0..4).fold(0u128, |a, k| a | (f(r) as u128) << (k * 32));
        match pick {
            0 => (self.next() as u128) << 64 | self.next() as u128,
            1 => lanes32(self, &|r| (((r.next() % 2000) as f32 - 1000.0) / 7.0).to_bits()),
            2 => {
                let a = (((self.next() % 2000) as f64 - 1000.0) / 3.0).to_bits();
                let b = (((self.next() % 2000) as f64 - 1000.0) / 9.0).to_bits();
                (a as u128) << 64 | b as u128
            }
            3 => lanes32(self, &|r| [0u32, 1, 0xff, 0x7f, 0x80, 0xffff, 0x8000, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff][(r.next() % 10) as usize]),
            4 => lanes32(self, &|r| [0.0f32, -0.0, 1.0, -1.5, f32::INFINITY, f32::NEG_INFINITY, f32::NAN, 1e-40, 3e38, 0.5][(r.next() % 10) as usize].to_bits()),
            _ => (0..16).fold(0u128, |a, k| a | ((self.next() % 256) as u128) << (k * 8)),
        }
    }
}

#[test]
fn sse_matches_cpu() {
    let mut rng = Rng(12345);
    let mut bad: Vec<String> = Vec::new();
    let mut tested = 0;
    for code in Code::values() {
        if code.op_code().encoding() != EncodingKind::Legacy {
            continue;
        }
        // xmm0, xmm1 の形と、即値つきの形を作れるものだけ
        let imms: &[u32] = &[0, 1, 2, 3, 4, 5, 6, 7, 0x1b, 0x4e, 0x55, 0xb1, 0xe4, 0x80, 0xff, 0x0f, 0x10];
        let forms: Vec<Instruction> = match Instruction::with2(code, Register::XMM0, Register::XMM1) {
            Ok(i) if i.op_count() == 2 => vec![i],
            _ => imms.iter().filter_map(|&k| Instruction::with3(code, Register::XMM0, Register::XMM1, k).ok()).filter(|i| i.op_count() == 3).collect(),
        };
        for ins in forms {
            // 近似の逆数は比べない
            if matches!(ins.mnemonic(), iced_x86::Mnemonic::Rcpps | iced_x86::Mnemonic::Rcpss | iced_x86::Mnemonic::Rsqrtps | iced_x86::Mnemonic::Rsqrtss) {
                continue;
            }
            if !supported(&ins) {
                continue;
            }
            let mut enc = Encoder::new(64);
            let Ok(_) = enc.encode(&ins, 0) else { continue };
            let bytes = enc.take_buffer();
            // エミュレーターが対応しているか（1 回目で確かめる）
            if emulated(&bytes, 0, 0).is_none() {
                continue;
            }
            tested += 1;
            let mut f = InstructionInfoFactory::new();
            let _ = f.info(&ins);
            // 比べるフラグ（命令が書くもののうち、CF、PF、ZF、SF、OF）
            let mask = (ins.rflags_modified() & (RflagsBits::CF | RflagsBits::PF | RflagsBits::ZF | RflagsBits::SF | RflagsBits::OF)) as u64;
            let fmask = (mask & 1) | (mask & 4) | (mask & 0x40) | (mask & 0x80) | (mask & 0x800);
            for _ in 0..300 {
                let (x0, x1) = (rng.xmm(), rng.xmm());
                let mut io = Io { x0, x1, ..Default::default() };
                native(&bytes, &mut io);
                let Some((got, gf)) = emulated(&bytes, x0, x1) else {
                    bad.push(format!("{ins}: エミュレーターが止まった x0={x0:032x} x1={x1:032x}"));
                    break;
                };
                if !same(got, io.out) || (gf & fmask) != (io.flags & fmask) {
                    bad.push(format!("{ins}: x0={x0:032x} x1={x1:032x} → CPU {:032x} フラグ {:x}、エミュレーター {got:032x} フラグ {:x}", io.out, io.flags & fmask, gf & fmask));
                    break;
                }
            }
        }
    }
    eprintln!("{tested} 個の形を試した");
    assert!(bad.is_empty(), "{} 個の命令が違う:\n{}", bad.len(), bad.join("\n"));
}
