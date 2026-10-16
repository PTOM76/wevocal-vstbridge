//! x87 の FPU の命令を 1 つ実行する。古い 32 ビットのコードは浮動小数点を x87（8 本のレジスタスタック）で計算する。
//! レジスタは本物は 80 ビットだが、ここでは f64 で持つ（音の処理の精度には十分）
use crate::cpu::Cpu;
use crate::mem::{Fault, Memory, R};
use iced_x86::{Instruction, Mnemonic as M, OpKind, Register};

/// st(n) のレジスタ番号（ST0=0…ST7=7）。レジスタの引数でなければ None
fn streg(r: Register) -> Option<usize> {
    if (Register::ST0..=Register::ST7).contains(&r) {
        Some(r as usize - Register::ST0 as usize)
    } else {
        None
    }
}

/// メモリの引数から浮動小数点か整数を読む（memory_size で幅を見分ける）
fn read_mem(cpu: &Cpu, mem: &Memory, i: &Instruction) -> R<f64> {
    let a = cpu.addr(i)?;
    let size = i.memory_size().size();
    Ok(match size {
        4 => {
            // f32 か i32。浮動小数点の命令（fld、fadd…）は f32、整数の命令（fild…）は i32。mnemonic で見分ける
            if is_int(i) {
                mem.read_n(a, 4)? as i32 as f64
            } else {
                f32::from_bits(mem.read_n(a, 4)? as u32) as f64
            }
        }
        8 => {
            if is_int(i) {
                mem.read_n(a, 8)? as i64 as f64
            } else {
                f64::from_bits(mem.read_n(a, 8)?)
            }
        }
        2 => mem.read_n(a, 2)? as i16 as f64, // fild word
        _ => return Err(Fault(format!("x87 mem size {size}"))),
    })
}

/// 整数を扱う命令か（fild、fist、fistp、fisttp、fiadd など整数メモリ相手の演算と比較）
fn is_int(i: &Instruction) -> bool {
    matches!(
        i.mnemonic(),
        M::Fild | M::Fist | M::Fistp | M::Fisttp | M::Fiadd | M::Fisub | M::Fisubr | M::Fimul | M::Fidiv | M::Fidivr | M::Ficom | M::Ficomp
    )
}

/// x87 の命令を実行する。扱わない命令なら Ok(false)
pub fn exec(cpu: &mut Cpu, mem: &mut Memory, i: &Instruction) -> R<bool> {
    let m = i.mnemonic();
    // 引数が「メモリか」「st(n) か」。多くの x87 命令は引数 0 がその対象
    let mem_op = i.op_count() > 0 && i.op_kind(0) == OpKind::Memory;
    match m {
        // 読み込み（スタックに積む）
        M::Fld | M::Fild => {
            let v = if mem_op { read_mem(cpu, mem, i)? } else { cpu.st(streg(i.op_register(0)).unwrap_or(0)) };
            cpu.fpush(v);
        }
        M::Fld1 => cpu.fpush(1.0),
        M::Fldz => cpu.fpush(0.0),
        // 書き出し（fst は残す、fstp は下ろす）
        M::Fst | M::Fstp | M::Fist | M::Fistp | M::Fisttp => {
            let v = cpu.st(0);
            if mem_op {
                let a = cpu.addr(i)?;
                let size = i.memory_size().size();
                if is_int(i) {
                    // fisttp は 0 方向への切り捨て、fist/fistp は丸め（最も近い）
                    let n = if m == M::Fisttp { v.trunc() } else { v.round_ties_even() } as i64;
                    mem.write_n(a, size, n as u64)?;
                } else if size == 4 {
                    let f = v as f32;
                    if !f.is_finite() && std::env::var("VSTBRIDGE_NAN").is_ok() {
                        eprintln!("[nan] f32 に書いた値が非有限: {i} @ {:#x} → {a:#x} に {f}（元の値 {v}）  steps={}", i.ip(), cpu.steps);
                    }
                    mem.write_n(a, 4, f.to_bits() as u64)?;
                } else {
                    mem.write_n(a, 8, v.to_bits())?;
                }
            } else {
                let n = streg(i.op_register(0)).unwrap_or(0);
                cpu.set_st(n, v);
            }
            if matches!(m, M::Fstp | M::Fistp | M::Fisttp) {
                cpu.fpop();
            }
        }
        // 四則。メモリ版は st(0) 相手、レジスタ版は st(0) と st(i)、p 付きは計算して pop。fi 系は整数メモリ相手
        M::Fadd | M::Fsub | M::Fsubr | M::Fmul | M::Fdiv | M::Fdivr | M::Faddp | M::Fsubp | M::Fsubrp | M::Fmulp | M::Fdivp | M::Fdivrp | M::Fiadd | M::Fisub | M::Fisubr | M::Fimul | M::Fidiv | M::Fidivr => {
            arith(cpu, mem, i)?;
        }
        // 整数メモリ相手の比較
        M::Ficom | M::Ficomp => {
            let b = read_mem(cpu, mem, i)?;
            let a = cpu.st(0);
            cpu.fcom(a, b);
            if m == M::Ficomp {
                cpu.fpop();
            }
        }
        M::Fchs => {
            let v = -cpu.st(0);
            cpu.set_st(0, v);
        }
        M::Fabs => {
            let v = cpu.st(0).abs();
            cpu.set_st(0, v);
        }
        M::Fsqrt => {
            let v = cpu.st(0).sqrt();
            cpu.set_st(0, v);
        }
        M::Frndint => {
            let v = cpu.st(0).round_ties_even();
            cpu.set_st(0, v);
        }
        // 超越関数（st(0) を書き換える）
        M::Fsin => {
            let v = cpu.st(0).sin();
            cpu.set_st(0, v);
        }
        M::Fcos => {
            let v = cpu.st(0).cos();
            cpu.set_st(0, v);
        }
        M::Fsincos => {
            let v = cpu.st(0);
            cpu.set_st(0, v.sin());
            cpu.fpush(v.cos());
        }
        M::Fptan => {
            let v = cpu.st(0).tan();
            cpu.set_st(0, v);
            cpu.fpush(1.0);
        }
        M::Fpatan => {
            // atan(st(1) / st(0)) を st(1) に入れて pop
            let r = cpu.st(1).atan2(cpu.st(0));
            cpu.fpop();
            cpu.set_st(0, r);
        }
        // fyl2x: st(1) * log2(st(0)) を st(1) に入れて pop。fyl2xp1 は log2(st(0)+1)
        M::Fyl2x => {
            let r = cpu.st(1) * cpu.st(0).log2();
            cpu.fpop();
            cpu.set_st(0, r);
        }
        M::Fyl2xp1 => {
            let r = cpu.st(1) * (cpu.st(0) + 1.0).log2();
            cpu.fpop();
            cpu.set_st(0, r);
        }
        // f2xm1: 2^st(0) - 1
        M::F2xm1 => {
            let v = cpu.st(0).exp2() - 1.0;
            cpu.set_st(0, v);
        }
        // fscale: st(0) * 2^trunc(st(1))
        M::Fscale => {
            let v = cpu.st(0) * (cpu.st(1).trunc()).exp2();
            cpu.set_st(0, v);
        }
        // fprem、fprem1: st(0) を st(1) で割った余り
        M::Fprem | M::Fprem1 => {
            let r = cpu.st(0) % cpu.st(1);
            cpu.set_st(0, r);
            cpu.fpu_c = (false, false, false, false); // C2=0（割り切れた扱い）
        }
        // 定数の読み込み
        M::Fldpi => cpu.fpush(std::f64::consts::PI),
        M::Fldl2e => cpu.fpush(std::f64::consts::LOG2_E),
        M::Fldl2t => cpu.fpush(std::f64::consts::LOG2_10),
        M::Fldlg2 => cpu.fpush(std::f64::consts::LOG10_2),
        M::Fldln2 => cpu.fpush(std::f64::consts::LN_2),
        M::Fxch => {
            let n = if i.op_count() > 0 { streg(i.op_register(0)).unwrap_or(1) } else { 1 };
            let (a, b) = (cpu.st(0), cpu.st(n));
            cpu.set_st(0, b);
            cpu.set_st(n, a);
        }
        // 比較（C3、C2、C0 を決める）
        M::Fcom | M::Fcomp | M::Fcompp | M::Fucom | M::Fucomp | M::Fucompp => {
            let b = if mem_op {
                read_mem(cpu, mem, i)?
            } else if i.op_count() > 0 {
                cpu.st(streg(i.op_register(0)).unwrap_or(1))
            } else {
                cpu.st(1)
            };
            let a = cpu.st(0);
            cpu.fcom(a, b);
            if matches!(m, M::Fcomp | M::Fucomp) {
                cpu.fpop();
            }
            if matches!(m, M::Fcompp | M::Fucompp) {
                cpu.fpop();
                cpu.fpop();
            }
        }
        // 比較して、結果を EFLAGS に入れる（fcomi、fucomi、p 付きは pop）
        M::Fcomi | M::Fcomip | M::Fucomi | M::Fucomip => {
            let b = cpu.st(streg(i.op_register(if i.op_count() > 1 { 1 } else { 0 })).unwrap_or(1));
            let a = cpu.st(0);
            if a.is_nan() || b.is_nan() {
                cpu.flags.zf = true;
                cpu.flags.pf = true;
                cpu.flags.cf = true;
            } else {
                cpu.flags.zf = a == b;
                cpu.flags.pf = false;
                cpu.flags.cf = a < b;
            }
            cpu.flags.of = false;
            cpu.flags.sf = false;
            if matches!(m, M::Fcomip | M::Fucomip) {
                cpu.fpop();
            }
        }
        // 状態ワードを ax（fnstsw ax）かメモリに書く
        M::Fnstsw => {
            let sw = cpu.fpu_status() as u64;
            if i.op_kind(0) == OpKind::Register {
                cpu.set_reg(i.op_register(0), sw)?;
            } else {
                let a = cpu.addr(i)?;
                mem.write_n(a, 2, sw)?;
            }
        }
        // 制御ワードの読み書きは、丸めの設定などだが、ここでは何もしない（既定の丸めで計算する）
        M::Fnstcw => {
            let a = cpu.addr(i)?;
            mem.write_n(a, 2, 0x037f)?; // 既定の制御ワード
        }
        M::Fldcw | M::Fnclex | M::Fninit | M::Wait => {}
        _ => return Ok(false),
    }
    Ok(true)
}

/// 四則演算。メモリ相手（st(0) op= mem）、レジスタ相手（st(0) op= st(i) か逆）、p 付き（計算して pop）
fn arith(cpu: &mut Cpu, mem: &mut Memory, i: &Instruction) -> R<()> {
    let m = i.mnemonic();
    // 逆向き（subr、divr）は b op a にする
    let reverse = matches!(m, M::Fsubr | M::Fsubrp | M::Fdivr | M::Fdivrp);
    let op = |a: f64, b: f64| -> f64 {
        match m {
            M::Fadd | M::Faddp | M::Fiadd => a + b,
            M::Fmul | M::Fmulp | M::Fimul => a * b,
            M::Fsub | M::Fsubp | M::Fisub => a - b,
            M::Fsubr | M::Fsubrp | M::Fisubr => b - a,
            M::Fdiv | M::Fdivp | M::Fidiv => a / b,
            M::Fdivr | M::Fdivrp | M::Fidivr => b / a,
            _ => unreachable!(),
        }
    };
    let _ = reverse;
    if i.op_count() > 0 && i.op_kind(0) == OpKind::Memory {
        // st(0) = st(0) op mem
        let b = read_mem(cpu, mem, i)?;
        let a = cpu.st(0);
        cpu.set_st(0, op(a, b));
        return Ok(());
    }
    // レジスタ相手。faddp st(i),st(0) のように、宛先が st(i)、もう一方が st(0)
    let p = matches!(m, M::Faddp | M::Fsubp | M::Fsubrp | M::Fmulp | M::Fdivp | M::Fdivrp);
    let (dst, src) = if i.op_count() == 2 {
        (streg(i.op_register(0)).unwrap_or(0), streg(i.op_register(1)).unwrap_or(0))
    } else if p {
        (1, 0) // faddp（引数なし）は st(1) = st(1) op st(0) のあと pop
    } else {
        (0, 1) // fadd（引数なし）相当は少ない
    };
    let r = op(cpu.st(dst), cpu.st(src));
    cpu.set_st(dst, r);
    if p {
        cpu.fpop();
    }
    Ok(())
}
