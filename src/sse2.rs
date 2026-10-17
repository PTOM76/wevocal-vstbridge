//! SSE と SSE2 の命令のうち、sse.rs に無いもの（整数の packed、並べ替え、比較、変換、f64 の packed）。64 ビットのプラグインは x87 を使わず、これらで計算する
use crate::cpu::{xmm_index, Cpu};
use crate::mem::{Memory, R};
use iced_x86::{Instruction, Mnemonic as M, OpKind};

/// 128 ビットを、bits ビットずつの値に分ける（下位から）
fn lanes(v: u128, bits: u32) -> Vec<u64> {
    let m = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    (0..128 / bits).map(|k| (v >> (k * bits)) as u64 & m).collect()
}
/// lanes の逆
fn join(xs: &[u64], bits: u32) -> u128 {
    let m = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    xs.iter().enumerate().fold(0u128, |a, (k, &x)| a | (((x & m) as u128) << (k as u32 * bits)))
}
/// 2 つの値の、同じ位置どうしに f をかける
fn map2(a: u128, b: u128, bits: u32, f: impl Fn(u64, u64) -> u64) -> u128 {
    let (x, y) = (lanes(a, bits), lanes(b, bits));
    join(&x.iter().zip(&y).map(|(&p, &q)| f(p, q)).collect::<Vec<_>>(), bits)
}
/// 符号付きにする
fn sx(v: u64, bits: u32) -> i64 {
    ((v << (64 - bits)) as i64) >> (64 - bits)
}
fn f32s(v: u128) -> [f32; 4] {
    [0, 1, 2, 3].map(|k| f32::from_bits((v >> (k * 32)) as u32))
}
fn from_f32s(x: [f32; 4]) -> u128 {
    x.iter().enumerate().fold(0u128, |a, (k, f)| a | ((f.to_bits() as u128) << (k * 32)))
}
fn f64s(v: u128) -> [f64; 2] {
    [f64::from_bits(v as u64), f64::from_bits((v >> 64) as u64)]
}
fn from_f64s(x: [f64; 2]) -> u128 {
    x[0].to_bits() as u128 | ((x[1].to_bits() as u128) << 64)
}
/// 飽和させて詰める（packsswb など）
fn sat(v: i64, lo: i64, hi: i64) -> u64 {
    v.clamp(lo, hi) as u64
}
/// cmpps などの比較（imm の下位 3 ビット）
fn cmp_f(a: f64, b: f64, imm: u8) -> bool {
    let un = a.is_nan() || b.is_nan();
    match imm & 7 {
        0 => a == b,
        1 => a < b,
        2 => a <= b,
        3 => un,
        4 => !(a == b),
        5 => !(a < b),
        6 => !(a <= b),
        _ => !un,
    }
}
/// 丸め（cvtps2dq などの、丸めの方式は既定の「最も近い偶数」）
fn round_even(f: f64) -> f64 {
    let r = f.round();
    if (f - f.trunc()).abs() == 0.5 { 2.0 * (f / 2.0).round() } else { r }
}
/// 範囲の外と NaN は 0x80000000（整数の不定値）
fn to_i32(f: f64) -> u64 {
    if f.is_nan() || f >= 2147483648.0 || f < -2147483648.0 { 0x8000_0000 } else { f as i32 as u32 as u64 }
}

fn imm(i: &Instruction) -> u8 {
    (0..i.op_count()).find(|&n| matches!(i.op_kind(n), OpKind::Immediate8)).map(|_| i.immediate8()).unwrap_or(0)
}

/// 扱わない命令なら Ok(false)
pub fn exec(cpu: &mut Cpu, mem: &mut Memory, i: &Instruction) -> R<bool> {
    let m = i.mnemonic();
    // 引数 0 が xmm のものだけ（MMX の同じ名前の命令は扱わない）
    if i.op_count() == 0 || i.op_kind(0) != OpKind::Register {
        return Ok(match m {
            // メモリへ書くもの
            M::Movhps | M::Movlps | M::Movhpd | M::Movlpd => {
                let v = cpu.read_xmm_op(mem, i, 1)?;
                let a = cpu.addr(i)?;
                let half = if matches!(m, M::Movhps | M::Movhpd) { (v >> 64) as u64 } else { v as u64 };
                mem.write_n(a, 8, half)?;
                true
            }
            // MXCSR は既定の値（例外はすべて隠す、最も近い偶数への丸め）だけを持ち、書き込みは無視する
            M::Stmxcsr => {
                let a = cpu.addr(i)?;
                mem.write_n(a, 4, 0x1f80)?;
                true
            }
            M::Ldmxcsr => true,
            M::Movntps | M::Movntdq | M::Movntpd => {
                let v = cpu.read_xmm_op(mem, i, 1)?;
                let a = cpu.addr(i)?;
                mem.write_u128(a, v)?;
                true
            }
            _ => false,
        });
    }
    let r0 = i.op_register(0);
    // 汎用レジスタに結果を返すもの
    match m {
        M::Movmskps | M::Movmskpd | M::Pmovmskb | M::Pextrw | M::Cvtss2si | M::Cvtsd2si if xmm_index(r0).is_err() => {
            let s = cpu.read_xmm_op(mem, i, 1)?;
            let v = match m {
                M::Movmskps => lanes(s, 32).iter().enumerate().fold(0, |a, (k, x)| a | ((x >> 31) << k)),
                M::Movmskpd => lanes(s, 64).iter().enumerate().fold(0, |a, (k, x)| a | ((x >> 63) << k)),
                M::Pmovmskb => lanes(s, 8).iter().enumerate().fold(0, |a, (k, x)| a | ((x >> 7) << k)),
                M::Pextrw => lanes(s, 16)[(imm(i) & 7) as usize],
                _ => {
                    let f = if m == M::Cvtss2si { f32s(s)[0] as f64 } else { f64s(s)[0] };
                    let r = round_even(f);
                    if r0.size() == 8 { if r.is_nan() || r.abs() >= 9.2e18 { 1u64 << 63 } else { r as i64 as u64 } } else { to_i32(r) }
                }
            };
            cpu.set_reg(r0, v)?;
            return Ok(true);
        }
        _ => {}
    }
    let Ok(x) = xmm_index(r0) else { return Ok(false) };
    let d = cpu.xmm[x];
    let src = |cpu: &Cpu, mem: &Memory| -> R<u128> { cpu.read_xmm_op(mem, i, 1) };
    // シフトの量（xmm、メモリ、即値）
    let count = |cpu: &Cpu, mem: &Memory| -> R<u64> { Ok(if matches!(i.op_kind(1), OpKind::Immediate8) { i.immediate8() as u64 } else { src(cpu, mem)? as u64 }) };
    let v: u128 = match m {
        // 整数の足し引き
        M::Paddb | M::Paddw | M::Paddd | M::Paddq | M::Psubb | M::Psubw | M::Psubd | M::Psubq => {
            let bits = match m {
                M::Paddb | M::Psubb => 8,
                M::Paddw | M::Psubw => 16,
                M::Paddd | M::Psubd => 32,
                _ => 64,
            };
            let sub = matches!(m, M::Psubb | M::Psubw | M::Psubd | M::Psubq);
            map2(d, src(cpu, mem)?, bits, |a, b| if sub { a.wrapping_sub(b) } else { a.wrapping_add(b) })
        }
        M::Paddsw | M::Psubsw => map2(d, src(cpu, mem)?, 16, |a, b| sat(if m == M::Paddsw { sx(a, 16) + sx(b, 16) } else { sx(a, 16) - sx(b, 16) }, -32768, 32767)),
        M::Paddusb | M::Psubusb => map2(d, src(cpu, mem)?, 8, |a, b| sat(if m == M::Paddusb { a as i64 + b as i64 } else { a as i64 - b as i64 }, 0, 255)),
        M::Paddusw | M::Psubusw => map2(d, src(cpu, mem)?, 16, |a, b| sat(if m == M::Paddusw { a as i64 + b as i64 } else { a as i64 - b as i64 }, 0, 65535)),
        M::Pmullw => map2(d, src(cpu, mem)?, 16, |a, b| a.wrapping_mul(b)),
        M::Pmulhw => map2(d, src(cpu, mem)?, 16, |a, b| ((sx(a, 16) * sx(b, 16)) >> 16) as u64),
        M::Pmulhuw => map2(d, src(cpu, mem)?, 16, |a, b| (a * b) >> 16),
        M::Pmuludq => {
            let s = src(cpu, mem)?;
            (((d as u32 as u64) * (s as u32 as u64)) as u128) | ((((d >> 64) as u32 as u64) * ((s >> 64) as u32 as u64)) as u128) << 64
        }
        M::Pmaddwd => {
            let (a, b) = (lanes(d, 16), lanes(src(cpu, mem)?, 16));
            join(&(0..4).map(|k| (sx(a[2 * k], 16) * sx(b[2 * k], 16) + sx(a[2 * k + 1], 16) * sx(b[2 * k + 1], 16)) as u64).collect::<Vec<_>>(), 32)
        }
        M::Pavgb => map2(d, src(cpu, mem)?, 8, |a, b| (a + b + 1) >> 1),
        M::Pavgw => map2(d, src(cpu, mem)?, 16, |a, b| (a + b + 1) >> 1),
        M::Pminub => map2(d, src(cpu, mem)?, 8, |a, b| a.min(b)),
        M::Pmaxub => map2(d, src(cpu, mem)?, 8, |a, b| a.max(b)),
        M::Pminsw => map2(d, src(cpu, mem)?, 16, |a, b| if sx(a, 16) < sx(b, 16) { a } else { b }),
        M::Pmaxsw => map2(d, src(cpu, mem)?, 16, |a, b| if sx(a, 16) > sx(b, 16) { a } else { b }),
        M::Psadbw => {
            let (a, b) = (lanes(d, 8), lanes(src(cpu, mem)?, 8));
            let s = |o: usize| (o..o + 8).map(|k| (a[k] as i64 - b[k] as i64).unsigned_abs()).sum::<u64>();
            s(0) as u128 | (s(8) as u128) << 64
        }
        // 比較（真なら全部 1）
        M::Pcmpeqb | M::Pcmpeqw | M::Pcmpeqd | M::Pcmpgtb | M::Pcmpgtw | M::Pcmpgtd => {
            let bits = match m {
                M::Pcmpeqb | M::Pcmpgtb => 8,
                M::Pcmpeqw | M::Pcmpgtw => 16,
                _ => 32,
            };
            let eq = matches!(m, M::Pcmpeqb | M::Pcmpeqw | M::Pcmpeqd);
            map2(d, src(cpu, mem)?, bits, |a, b| if (eq && a == b) || (!eq && sx(a, bits) > sx(b, bits)) { u64::MAX } else { 0 })
        }
        // シフト
        M::Psllw | M::Pslld | M::Psllq | M::Psrlw | M::Psrld | M::Psrlq | M::Psraw | M::Psrad => {
            let n = count(cpu, mem)?;
            let bits = match m {
                M::Psllw | M::Psrlw | M::Psraw => 16,
                M::Pslld | M::Psrld | M::Psrad => 32,
                _ => 64,
            };
            let xs: Vec<u64> = lanes(d, bits)
                .into_iter()
                .map(|a| match m {
                    M::Psraw | M::Psrad => (sx(a, bits) >> n.min(bits as u64 - 1)) as u64,
                    _ if n >= bits as u64 => 0,
                    M::Psllw | M::Pslld | M::Psllq => a << n,
                    _ => a >> n,
                })
                .collect();
            join(&xs, bits)
        }
        M::Pslldq => {
            let n = i.immediate8() as u32;
            if n >= 16 { 0 } else { d << (n * 8) }
        }
        M::Psrldq => {
            let n = i.immediate8() as u32;
            if n >= 16 { 0 } else { d >> (n * 8) }
        }
        // 並べ替え
        M::Pshufd => {
            let (s, k) = (lanes(src(cpu, mem)?, 32), imm(i));
            join(&(0..4).map(|j| s[((k >> (j * 2)) & 3) as usize]).collect::<Vec<_>>(), 32)
        }
        M::Pshuflw | M::Pshufhw => {
            let (s, k) = (lanes(src(cpu, mem)?, 16), imm(i));
            let o = if m == M::Pshuflw { 0 } else { 4 };
            let mut out = s.clone();
            for j in 0..4 {
                out[o + j] = s[o + ((k >> (j * 2)) & 3) as usize];
            }
            join(&out, 16)
        }
        M::Shufps => {
            let (a, b, k) = (lanes(d, 32), lanes(src(cpu, mem)?, 32), imm(i));
            join(&[a[(k & 3) as usize], a[((k >> 2) & 3) as usize], b[((k >> 4) & 3) as usize], b[((k >> 6) & 3) as usize]], 32)
        }
        M::Shufpd => {
            let (a, b, k) = (lanes(d, 64), lanes(src(cpu, mem)?, 64), imm(i));
            join(&[a[(k & 1) as usize], b[((k >> 1) & 1) as usize]], 64)
        }
        M::Punpcklbw | M::Punpcklwd | M::Punpckldq | M::Punpckhbw | M::Punpckhwd | M::Punpckhdq | M::Punpckhqdq | M::Unpckhps | M::Unpcklpd | M::Unpckhpd => {
            let bits = match m {
                M::Punpcklbw | M::Punpckhbw => 8,
                M::Punpcklwd | M::Punpckhwd => 16,
                M::Punpckldq | M::Punpckhdq | M::Unpckhps => 32,
                _ => 64,
            };
            let hi = matches!(m, M::Punpckhbw | M::Punpckhwd | M::Punpckhdq | M::Punpckhqdq | M::Unpckhps | M::Unpckhpd);
            let (a, b) = (lanes(d, bits), lanes(src(cpu, mem)?, bits));
            let half = a.len() / 2;
            let o = if hi { half } else { 0 };
            join(&(0..half).flat_map(|k| [a[o + k], b[o + k]]).collect::<Vec<_>>(), bits)
        }
        M::Packsswb | M::Packuswb | M::Packssdw => {
            let bits = if m == M::Packssdw { 32 } else { 16 };
            let (lo, hi) = match m {
                M::Packsswb => (-128, 127),
                M::Packuswb => (0, 255),
                _ => (-32768, 32767),
            };
            let xs: Vec<u64> = lanes(d, bits).into_iter().chain(lanes(src(cpu, mem)?, bits)).map(|a| sat(sx(a, bits), lo, hi)).collect();
            join(&xs, bits / 2)
        }
        M::Pinsrw => {
            let (n, _) = cpu.read_op(mem, i, 1)?;
            let k = (imm(i) & 7) as u32 * 16;
            (d & !(0xffffu128 << k)) | (((n & 0xffff) as u128) << k)
        }
        M::Movhlps => (d & !((1u128 << 64) - 1)) | (src(cpu, mem)? >> 64),
        M::Movlhps => (d & ((1u128 << 64) - 1)) | (src(cpu, mem)? << 64),
        M::Movhps | M::Movhpd => (d & ((1u128 << 64) - 1)) | ((src(cpu, mem)? & ((1u128 << 64) - 1)) << 64),
        M::Movlps | M::Movlpd => (d & !((1u128 << 64) - 1)) | (src(cpu, mem)? & ((1u128 << 64) - 1)),
        M::Movddup => {
            let s = src(cpu, mem)? & ((1u128 << 64) - 1);
            s | (s << 64)
        }
        M::Movntps | M::Movntdq | M::Movntpd | M::Lddqu => src(cpu, mem)?,
        // f32 の packed
        M::Minps | M::Maxps => {
            let (a, b) = (f32s(d), f32s(src(cpu, mem)?));
            from_f32s([0, 1, 2, 3].map(|k| if (m == M::Minps && a[k] < b[k]) || (m == M::Maxps && a[k] > b[k]) { a[k] } else { b[k] }))
        }
        M::Sqrtps => from_f32s(f32s(src(cpu, mem)?).map(f32::sqrt)),
        M::Rcpps => from_f32s(f32s(src(cpu, mem)?).map(|f| 1.0 / f)),
        M::Rsqrtps => from_f32s(f32s(src(cpu, mem)?).map(|f| 1.0 / f.sqrt())),
        M::Rcpss => (d & !0xffff_ffffu128) | (1.0 / f32s(src(cpu, mem)?)[0]).to_bits() as u128,
        M::Rsqrtss => (d & !0xffff_ffffu128) | (1.0 / f32s(src(cpu, mem)?)[0].sqrt()).to_bits() as u128,
        M::Cmpps => {
            let (a, b, k) = (f32s(d), f32s(src(cpu, mem)?), imm(i));
            join(&(0..4).map(|j| if cmp_f(a[j] as f64, b[j] as f64, k) { u64::MAX } else { 0 }).collect::<Vec<_>>(), 32)
        }
        M::Cmpss => {
            let (a, b) = (f32s(d)[0] as f64, f32s(src(cpu, mem)?)[0] as f64);
            (d & !0xffff_ffffu128) | if cmp_f(a, b, imm(i)) { 0xffff_ffff } else { 0 }
        }
        M::Cmppd => {
            let (a, b, k) = (f64s(d), f64s(src(cpu, mem)?), imm(i));
            join(&(0..2).map(|j| if cmp_f(a[j], b[j], k) { u64::MAX } else { 0 }).collect::<Vec<_>>(), 64)
        }
        M::Cmpsd => {
            let (a, b) = (f64s(d)[0], f64s(src(cpu, mem)?)[0]);
            (d & !((1u128 << 64) - 1)) | if cmp_f(a, b, imm(i)) { u64::MAX as u128 } else { 0 }
        }
        // f64 の packed と 1 つ
        M::Addpd | M::Subpd | M::Mulpd | M::Divpd | M::Minpd | M::Maxpd => {
            let (a, b) = (f64s(d), f64s(src(cpu, mem)?));
            from_f64s([0, 1].map(|k| match m {
                M::Addpd => a[k] + b[k],
                M::Subpd => a[k] - b[k],
                M::Mulpd => a[k] * b[k],
                M::Divpd => a[k] / b[k],
                M::Minpd => if a[k] < b[k] { a[k] } else { b[k] },
                _ => if a[k] > b[k] { a[k] } else { b[k] },
            }))
        }
        M::Sqrtpd => from_f64s(f64s(src(cpu, mem)?).map(f64::sqrt)),
        M::Minsd | M::Maxsd => {
            let (a, b) = (f64s(d)[0], f64s(src(cpu, mem)?)[0]);
            let r = if (m == M::Minsd && a < b) || (m == M::Maxsd && a > b) { a } else { b };
            (d & !((1u128 << 64) - 1)) | r.to_bits() as u128
        }
        // 変換
        M::Cvtdq2ps => from_f32s(lanes(src(cpu, mem)?, 32).iter().map(|&a| sx(a, 32) as f32).collect::<Vec<_>>().try_into().unwrap()),
        M::Cvtps2dq | M::Cvttps2dq => join(&f32s(src(cpu, mem)?).map(|f| to_i32(if m == M::Cvtps2dq { round_even(f as f64) } else { (f as f64).trunc() })), 32),
        M::Cvtdq2pd => {
            let s = lanes(src(cpu, mem)?, 32);
            from_f64s([sx(s[0], 32) as f64, sx(s[1], 32) as f64])
        }
        M::Cvtpd2dq | M::Cvttpd2dq => {
            let s = f64s(src(cpu, mem)?);
            join(&[to_i32(if m == M::Cvtpd2dq { round_even(s[0]) } else { s[0].trunc() }), to_i32(if m == M::Cvtpd2dq { round_even(s[1]) } else { s[1].trunc() }), 0, 0], 32)
        }
        M::Cvtps2pd => {
            let s = f32s(src(cpu, mem)?);
            from_f64s([s[0] as f64, s[1] as f64])
        }
        M::Cvtpd2ps => {
            let s = f64s(src(cpu, mem)?);
            from_f32s([s[0] as f32, s[1] as f32, 0.0, 0.0])
        }
        _ => return Ok(false),
    };
    cpu.xmm[x] = v;
    Ok(true)
}
