//! SSE の命令（浮動小数点と 128 ビットの移動）を 1 つ実行する。音の処理で使うものから足していく
use crate::cpu::{xmm_index, Cpu};
use crate::mem::{Memory, R};
use iced_x86::{Instruction, Mnemonic as M, OpKind};

const LO32: u128 = 0xffff_ffff;
const LO64: u128 = 0xffff_ffff_ffff_ffff;

fn f32_of(v: u128) -> f32 {
    f32::from_bits(v as u32)
}
fn f64_of(v: u128) -> f64 {
    f64::from_bits(v as u64)
}

/// 4 つの f32 に同じ計算をする（packed）
fn packed(a: u128, b: u128, f: impl Fn(f32, f32) -> f32) -> u128 {
    let mut out = 0u128;
    for k in 0..4 {
        let x = f32::from_bits((a >> (k * 32)) as u32);
        let y = f32::from_bits((b >> (k * 32)) as u32);
        out |= (f(x, y).to_bits() as u128) << (k * 32);
    }
    out
}

fn is_xmm(i: &Instruction, n: u32) -> bool {
    i.op_kind(n) == OpKind::Register && xmm_index(i.op_register(n)).is_ok()
}

/// 引数 0 に 128 ビットを書く（xmm か、メモリなら memory_size だけ。汎用レジスタなら下位）
fn write(cpu: &mut Cpu, mem: &mut Memory, i: &Instruction, v: u128) -> R<()> {
    match i.op_kind(0) {
        OpKind::Register => match xmm_index(i.op_register(0)) {
            Ok(x) => {
                cpu.xmm[x] = v;
                Ok(())
            }
            Err(_) => cpu.set_reg(i.op_register(0), v as u64),
        },
        _ => {
            let a = cpu.addr(i)?;
            let size = i.memory_size().size();
            if size >= 16 { mem.write_u128(a, v) } else { mem.write_n(a, size, v as u64) }
        }
    }
}

/// SSE の命令を実行する。扱わない命令なら Ok(false)
pub fn exec(cpu: &mut Cpu, mem: &mut Memory, i: &Instruction) -> R<bool> {
    let m = i.mnemonic();
    let dst = || -> R<u128> { cpu.read_xmm_op(mem, i, 0) };
    let src = || -> R<u128> { cpu.read_xmm_op(mem, i, 1) };
    let v = match m {
        // 移動
        M::Movaps | M::Movups | M::Movdqa | M::Movdqu | M::Movapd | M::Movupd => src()?,
        M::Movss if is_xmm(i, 0) && is_xmm(i, 1) => (dst()? & !LO32) | (src()? & LO32),
        M::Movsd if is_xmm(i, 0) && is_xmm(i, 1) => (dst()? & !LO64) | (src()? & LO64),
        M::Movss => src()? & LO32,
        M::Movsd if i.op_kind(0) == OpKind::Register || i.op_kind(1) == OpKind::Register => src()? & LO64,
        M::Movq => src()? & LO64,
        M::Movd => src()? & LO32,
        // ビット演算
        M::Pxor | M::Xorps | M::Xorpd => dst()? ^ src()?,
        M::Pand | M::Andps | M::Andpd => dst()? & src()?,
        M::Pandn | M::Andnps | M::Andnpd => !dst()? & src()?,
        M::Por | M::Orps | M::Orpd => dst()? | src()?,
        M::Punpcklqdq => (dst()? & LO64) | ((src()? & LO64) << 64),
        M::Unpcklps => {
            let (a, b) = (dst()?, src()?);
            (a & LO32) | ((b & LO32) << 32) | (((a >> 32) & LO32) << 64) | (((b >> 32) & LO32) << 96)
        }
        // 浮動小数点（1 つ）
        M::Addss | M::Subss | M::Mulss | M::Divss | M::Minss | M::Maxss | M::Sqrtss => {
            let (a, b) = (f32_of(dst()?), f32_of(src()?));
            let r = match m {
                M::Addss => a + b,
                M::Subss => a - b,
                M::Mulss => a * b,
                M::Divss => a / b,
                M::Minss => if a < b { a } else { b },
                M::Maxss => if a > b { a } else { b },
                _ => b.sqrt(),
            };
            (dst()? & !LO32) | r.to_bits() as u128
        }
        M::Addsd | M::Subsd | M::Mulsd | M::Divsd | M::Sqrtsd => {
            let (a, b) = (f64_of(dst()?), f64_of(src()?));
            let r = match m {
                M::Addsd => a + b,
                M::Subsd => a - b,
                M::Mulsd => a * b,
                M::Divsd => a / b,
                _ => b.sqrt(),
            };
            (dst()? & !LO64) | r.to_bits() as u128
        }
        // 浮動小数点（4 つ）
        M::Addps => packed(dst()?, src()?, |a, b| a + b),
        M::Subps => packed(dst()?, src()?, |a, b| a - b),
        M::Mulps => packed(dst()?, src()?, |a, b| a * b),
        M::Divps => packed(dst()?, src()?, |a, b| a / b),
        // 変換
        M::Cvtsi2ss => {
            let (n, s) = cpu.read_op(mem, i, 1)?;
            let f = if s == 8 { n as i64 as f32 } else { n as u32 as i32 as f32 };
            (dst()? & !LO32) | f.to_bits() as u128
        }
        M::Cvtsi2sd => {
            let (n, s) = cpu.read_op(mem, i, 1)?;
            let f = if s == 8 { n as i64 as f64 } else { n as u32 as i32 as f64 };
            (dst()? & !LO64) | f.to_bits() as u128
        }
        M::Cvtss2sd => (dst()? & !LO64) | (f32_of(src()?) as f64).to_bits() as u128,
        M::Cvtsd2ss => (dst()? & !LO32) | (f64_of(src()?) as f32).to_bits() as u128,
        M::Cvttss2si | M::Cvttsd2si => {
            let f = if m == M::Cvttss2si { f32_of(src()?) as f64 } else { f64_of(src()?) };
            let size = i.op_register(0).size();
            let n = if size == 8 { f as i64 as u64 } else { f as i32 as u32 as u64 };
            cpu.set_reg(i.op_register(0), n)?;
            return Ok(true);
        }
        // 比較（フラグ）
        M::Comiss | M::Ucomiss | M::Comisd | M::Ucomisd => {
            let (a, b) = if matches!(m, M::Comiss | M::Ucomiss) { (f32_of(dst()?) as f64, f32_of(src()?) as f64) } else { (f64_of(dst()?), f64_of(src()?)) };
            let un = a.is_nan() || b.is_nan();
            cpu.flags.zf = un || a == b;
            cpu.flags.pf = un;
            cpu.flags.cf = un || a < b;
            cpu.flags.of = false;
            cpu.flags.sf = false;
            return Ok(true);
        }
        _ => return Ok(false),
    };
    write(cpu, mem, i, v)?;
    Ok(true)
}
