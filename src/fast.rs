//! 32 ビットのコードでよく出る命令を、引数を前もって解いた形（Op）にして速く動かす。
//! iced の命令を毎回たどる（レジスタの種類、メモリの基点、幅を引く）のをやめ、レジスタ番号や即値を直接持つ。
//! 対応しない形は Op::Generic にして、exec.rs と x87.rs の汎用の実装に任せる
use crate::cpu::{Cpu, Flags, RSP};
use crate::mem::{Memory, R};
use iced_x86::{CodeSize, ConditionCode, Instruction, Mnemonic as M, OpKind, Register};

const NONE: u8 = 0xff;

/// メモリの引数の番地の計算（基点 + 添字 × 倍率 + 変位。32 ビットで回り込む）
#[derive(Clone, Copy, Debug)]
pub struct Ea {
    base: u8,
    index: u8,
    shift: u8,
    fs: bool,
    disp: u32,
}

impl Ea {
    #[inline(always)]
    fn addr(&self, cpu: &Cpu) -> u64 {
        let mut a = self.disp;
        if self.base != NONE {
            a = a.wrapping_add(cpu.regs[self.base as usize] as u32);
        }
        if self.index != NONE {
            a = a.wrapping_add((cpu.regs[self.index as usize] as u32) << self.shift);
        }
        let mut a = a as u64;
        if self.fs {
            a = a.wrapping_add(cpu.fs_base) & 0xffff_ffff;
        }
        a
    }
}

/// 値の読み先（レジスタ、即値、メモリ）
#[derive(Clone, Copy, Debug)]
pub enum Src {
    R(u8),
    I(u32),
    M(Ea),
}

/// 値の書き先（レジスタ、メモリ）
#[derive(Clone, Copy, Debug)]
pub enum Dst {
    R(u8),
    M(Ea),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Alu {
    Add,
    Sub,
    Cmp,
    And,
    Or,
    Xor,
    Test,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FOp {
    Add,
    Mul,
    Sub,
    SubR,
    Div,
    DivR,
}

#[derive(Clone, Copy, Debug)]
pub enum Op {
    /// 前もって解けない形。汎用の実装で動かす
    Generic,
    Mov(Dst, Src),
    Lea(u8, Ea),
    Alu(Alu, Dst, Src),
    IncDec { dec: bool, d: Dst },
    Push(Src),
    Pop(Dst),
    Jcc(ConditionCode, u32),
    Jmp(u32),
    Call { target: u32, ret: u32 },
    Ret(u32),
    // x87
    FldM32(Ea),
    FldM64(Ea),
    FldSt(u8),
    FstM32 { ea: Ea, pop: bool },
    FstM64 { ea: Ea, pop: bool },
    FstSt { n: u8, pop: bool },
    FArithM { op: FOp, ea: Ea, wide: bool },
    FArithSt { op: FOp, dst: u8, src: u8, pop: bool },
    Fxch(u8),
    Fchs,
    Fabs,
    Fld1,
    Fldz,
}

fn gpr32(r: Register) -> Option<u8> {
    if r.is_gpr32() {
        Some((r as usize - Register::EAX as usize) as u8)
    } else {
        None
    }
}

fn st(r: Register) -> Option<u8> {
    if (Register::ST0..=Register::ST7).contains(&r) {
        Some((r as usize - Register::ST0 as usize) as u8)
    } else {
        None
    }
}

/// メモリの引数 → Ea（16 ビットのアドレス指定、fs 以外のセグメントなどは None）
fn ea(i: &Instruction) -> Option<Ea> {
    let base = match i.memory_base() {
        Register::None => NONE,
        r => gpr32(r)?,
    };
    let index = match i.memory_index() {
        Register::None => NONE,
        r => gpr32(r)?,
    };
    let fs = match i.segment_prefix() {
        Register::FS => true,
        Register::None | Register::DS | Register::SS | Register::ES | Register::CS => false,
        _ => return None,
    };
    let shift = match i.memory_index_scale() {
        1 => 0,
        2 => 1,
        4 => 2,
        8 => 3,
        _ => return None,
    };
    Some(Ea { base, index, shift, fs, disp: i.memory_displacement64() as u32 })
}

/// 4 バイトの引数 n を読み先にする
fn src(i: &Instruction, n: u32) -> Option<Src> {
    match i.op_kind(n) {
        OpKind::Register => gpr32(i.op_register(n)).map(Src::R),
        OpKind::Memory if i.memory_size().size() == 4 => ea(i).map(Src::M),
        OpKind::Immediate8 | OpKind::Immediate16 | OpKind::Immediate32 | OpKind::Immediate8to32 | OpKind::Immediate8to16 => Some(Src::I(i.immediate(n) as u32)),
        _ => None,
    }
}

fn dst(i: &Instruction, n: u32) -> Option<Dst> {
    match src(i, n)? {
        Src::R(r) => Some(Dst::R(r)),
        Src::M(e) => Some(Dst::M(e)),
        Src::I(_) => None,
    }
}

fn near_target(i: &Instruction) -> Option<u32> {
    match i.op_kind(0) {
        OpKind::NearBranch32 => Some(i.near_branch32()),
        _ => None,
    }
}

/// 命令を Op にする（32 ビットのコードだけ。対応しない形は Generic）
pub fn compile(i: &Instruction) -> Op {
    if i.code_size() != CodeSize::Code32 {
        return Op::Generic;
    }
    compile_inner(i).unwrap_or(Op::Generic)
}

fn compile_inner(i: &Instruction) -> Option<Op> {
    let m = i.mnemonic();
    // 4 バイトの整数の引数が 2 つ（mov、add…）。ただし移動と演算で、引数の幅が両方 4 バイトのものだけ
    Some(match m {
        M::Mov if i.op_count() == 2 => Op::Mov(dst(i, 0)?, src(i, 1)?),
        M::Lea => {
            let d = gpr32(i.op_register(0))?;
            Op::Lea(d, ea(i)?)
        }
        M::Add | M::Sub | M::Cmp | M::And | M::Or | M::Xor | M::Test if i.op_count() == 2 => {
            let k = match m {
                M::Add => Alu::Add,
                M::Sub => Alu::Sub,
                M::Cmp => Alu::Cmp,
                M::And => Alu::And,
                M::Or => Alu::Or,
                M::Xor => Alu::Xor,
                _ => Alu::Test,
            };
            Op::Alu(k, dst(i, 0)?, src(i, 1)?)
        }
        M::Inc | M::Dec => Op::IncDec { dec: m == M::Dec, d: dst(i, 0)? },
        M::Push if i.op_count() == 1 => Op::Push(src(i, 0)?),
        M::Pop if i.op_count() == 1 => Op::Pop(dst(i, 0)?),
        M::Jmp => Op::Jmp(near_target(i)?),
        M::Call => Op::Call { target: near_target(i)?, ret: i.next_ip() as u32 },
        M::Ret => Op::Ret(if i.op_count() == 1 { i.immediate(0) as u32 } else { 0 }),
        _ if i.condition_code() != ConditionCode::None && i.op_count() == 1 && i.op_kind(0) == OpKind::NearBranch32 => Op::Jcc(i.condition_code(), i.near_branch32()),
        // ---- x87 ----
        M::Fld => match i.op_kind(0) {
            OpKind::Memory => match i.memory_size().size() {
                4 => Op::FldM32(ea(i)?),
                8 => Op::FldM64(ea(i)?),
                _ => return None,
            },
            OpKind::Register => Op::FldSt(st(i.op_register(0))?),
            _ => return None,
        },
        M::Fst | M::Fstp => {
            let pop = m == M::Fstp;
            match i.op_kind(0) {
                OpKind::Memory => match i.memory_size().size() {
                    4 => Op::FstM32 { ea: ea(i)?, pop },
                    8 => Op::FstM64 { ea: ea(i)?, pop },
                    _ => return None,
                },
                OpKind::Register => Op::FstSt { n: st(i.op_register(0))?, pop },
                _ => return None,
            }
        }
        M::Fadd | M::Fmul | M::Fsub | M::Fsubr | M::Fdiv | M::Fdivr | M::Faddp | M::Fmulp | M::Fsubp | M::Fsubrp | M::Fdivp | M::Fdivrp => {
            let op = match m {
                M::Fadd | M::Faddp => FOp::Add,
                M::Fmul | M::Fmulp => FOp::Mul,
                M::Fsub | M::Fsubp => FOp::Sub,
                M::Fsubr | M::Fsubrp => FOp::SubR,
                M::Fdiv | M::Fdivp => FOp::Div,
                _ => FOp::DivR,
            };
            let pop = matches!(m, M::Faddp | M::Fmulp | M::Fsubp | M::Fsubrp | M::Fdivp | M::Fdivrp);
            if i.op_count() == 0 {
                return None;
            }
            if i.op_kind(0) == OpKind::Memory {
                let wide = match i.memory_size().size() {
                    4 => false,
                    8 => true,
                    _ => return None,
                };
                Op::FArithM { op, ea: ea(i)?, wide }
            } else if i.op_count() == 2 {
                Op::FArithSt { op, dst: st(i.op_register(0))?, src: st(i.op_register(1))?, pop }
            } else {
                return None;
            }
        }
        M::Fxch => {
            let r = if i.op_count() > 1 { i.op_register(1) } else if i.op_count() == 1 { i.op_register(0) } else { return None };
            Op::Fxch(st(r)?)
        }
        M::Fchs => Op::Fchs,
        M::Fabs => Op::Fabs,
        M::Fld1 => Op::Fld1,
        M::Fldz => Op::Fldz,
        _ => return None,
    })
}

#[inline(always)]
fn read(cpu: &Cpu, mem: &Memory, s: Src) -> R<u32> {
    Ok(match s {
        Src::R(r) => cpu.regs[r as usize] as u32,
        Src::I(v) => v,
        Src::M(e) => mem.read_u32(e.addr(cpu))?,
    })
}

#[inline(always)]
fn write(cpu: &mut Cpu, mem: &mut Memory, d: Dst, v: u32) -> R<()> {
    match d {
        Dst::R(r) => {
            cpu.regs[r as usize] = v as u64;
            Ok(())
        }
        Dst::M(e) => mem.write_u32(e.addr(cpu), v),
    }
}

#[inline(always)]
fn szp(f: &mut Flags, r: u32) {
    f.zf = r == 0;
    f.sf = r >> 31 != 0;
    f.pf = (r as u8).count_ones() & 1 == 0;
}

#[inline(always)]
fn add32(f: &mut Flags, a: u32, b: u32) -> u32 {
    let (r, c) = a.overflowing_add(b);
    f.cf = c;
    f.of = ((a ^ r) & (b ^ r)) >> 31 != 0;
    szp(f, r);
    r
}

#[inline(always)]
fn sub32(f: &mut Flags, a: u32, b: u32) -> u32 {
    let (r, c) = a.overflowing_sub(b);
    f.cf = c;
    f.of = ((a ^ b) & (a ^ r)) >> 31 != 0;
    szp(f, r);
    r
}

#[inline(always)]
fn fop(op: FOp, a: f64, b: f64) -> f64 {
    match op {
        FOp::Add => a + b,
        FOp::Mul => a * b,
        FOp::Sub => a - b,
        FOp::SubR => b - a,
        FOp::Div => a / b,
        FOp::DivR => b / a,
    }
}

/// Op を実行する。cpu.rip は呼ぶ側が命令の次に進めてある（分岐は中で書き換える）
#[inline(always)]
pub fn run(cpu: &mut Cpu, mem: &mut Memory, op: Op) -> R<()> {
    match op {
        Op::Generic => unreachable!(),
        Op::Mov(d, s) => {
            let v = read(cpu, mem, s)?;
            write(cpu, mem, d, v)?;
        }
        Op::Lea(d, e) => cpu.regs[d as usize] = e.addr(cpu),
        Op::Alu(k, d, s) => {
            let a = read(cpu, mem, match d {
                Dst::R(r) => Src::R(r),
                Dst::M(e) => Src::M(e),
            })?;
            let b = read(cpu, mem, s)?;
            let r = match k {
                Alu::Add => add32(&mut cpu.flags, a, b),
                Alu::Sub | Alu::Cmp => sub32(&mut cpu.flags, a, b),
                Alu::And | Alu::Test | Alu::Or | Alu::Xor => {
                    let r = match k {
                        Alu::Or => a | b,
                        Alu::Xor => a ^ b,
                        _ => a & b,
                    };
                    cpu.flags.cf = false;
                    cpu.flags.of = false;
                    szp(&mut cpu.flags, r);
                    r
                }
            };
            if k != Alu::Cmp && k != Alu::Test {
                write(cpu, mem, d, r)?;
            }
        }
        Op::IncDec { dec, d } => {
            let a = read(cpu, mem, match d {
                Dst::R(r) => Src::R(r),
                Dst::M(e) => Src::M(e),
            })?;
            let cf = cpu.flags.cf;
            let r = if dec { sub32(&mut cpu.flags, a, 1) } else { add32(&mut cpu.flags, a, 1) };
            cpu.flags.cf = cf;
            write(cpu, mem, d, r)?;
        }
        Op::Push(s) => {
            let v = read(cpu, mem, s)?;
            let sp = (cpu.regs[RSP] as u32).wrapping_sub(4);
            cpu.regs[RSP] = sp as u64;
            mem.write_u32(sp as u64, v)?;
        }
        Op::Pop(d) => {
            let sp = cpu.regs[RSP] as u32;
            let v = mem.read_u32(sp as u64)?;
            cpu.regs[RSP] = sp.wrapping_add(4) as u64;
            write(cpu, mem, d, v)?;
        }
        Op::Jcc(cc, t) => {
            if cpu.cond(cc) {
                cpu.rip = t as u64;
            }
        }
        Op::Jmp(t) => cpu.rip = t as u64,
        Op::Call { target, ret } => {
            let sp = (cpu.regs[RSP] as u32).wrapping_sub(4);
            cpu.regs[RSP] = sp as u64;
            mem.write_u32(sp as u64, ret)?;
            cpu.rip = target as u64;
        }
        Op::Ret(n) => {
            let sp = cpu.regs[RSP] as u32;
            cpu.rip = mem.read_u32(sp as u64)? as u64;
            cpu.regs[RSP] = sp.wrapping_add(4).wrapping_add(n) as u64;
        }
        Op::FldM32(e) => {
            let v = f32::from_bits(mem.read_u32(e.addr(cpu))?) as f64;
            cpu.fpush(v);
        }
        Op::FldM64(e) => {
            let v = f64::from_bits(mem.read_u64(e.addr(cpu))?);
            cpu.fpush(v);
        }
        Op::FldSt(n) => {
            let v = cpu.st(n as usize);
            cpu.fpush(v);
        }
        Op::FstM32 { ea, pop } => {
            let v = cpu.st(0) as f32;
            mem.write_u32(ea.addr(cpu), v.to_bits())?;
            if pop {
                cpu.fpop();
            }
        }
        Op::FstM64 { ea, pop } => {
            let v = cpu.st(0);
            mem.write_u64(ea.addr(cpu), v.to_bits())?;
            if pop {
                cpu.fpop();
            }
        }
        Op::FstSt { n, pop } => {
            let v = cpu.st(0);
            cpu.set_st(n as usize, v);
            if pop {
                cpu.fpop();
            }
        }
        Op::FArithM { op, ea, wide } => {
            let b = if wide { f64::from_bits(mem.read_u64(ea.addr(cpu))?) } else { f32::from_bits(mem.read_u32(ea.addr(cpu))?) as f64 };
            let a = cpu.st(0);
            cpu.set_st(0, fop(op, a, b));
        }
        Op::FArithSt { op, dst, src, pop } => {
            let r = fop(op, cpu.st(dst as usize), cpu.st(src as usize));
            cpu.set_st(dst as usize, r);
            if pop {
                cpu.fpop();
            }
        }
        Op::Fxch(n) => {
            let (a, b) = (cpu.st(0), cpu.st(n as usize));
            cpu.set_st(0, b);
            cpu.set_st(n as usize, a);
        }
        Op::Fchs => {
            let v = -cpu.st(0);
            cpu.set_st(0, v);
        }
        Op::Fabs => {
            let v = cpu.st(0).abs();
            cpu.set_st(0, v);
        }
        Op::Fld1 => cpu.fpush(1.0),
        Op::Fldz => cpu.fpush(0.0),
    }
    Ok(())
}
