//! 整数の命令を 1 つ実行する（データの移動、四則演算、比較、分岐、呼び出し）。SSE は sse.rs
use crate::cpu::{mask, sext, sign, Cpu, RSP};
use crate::mem::{Fault, Memory, R};
use iced_x86::{Code, ConditionCode, Instruction, Mnemonic as M, OpKind};

/// 引数 0 の大きさ（バイト）
fn dst_size(i: &Instruction) -> usize {
    match i.op_kind(0) {
        OpKind::Register => i.op_register(0).size(),
        OpKind::Memory => i.memory_size().size(),
        _ => 8,
    }
}

/// 足し算と引き算（cmp も）。結果とフラグ
fn add_sub(cpu: &mut Cpu, a: u64, b: u64, size: usize, sub: bool) -> u64 {
    let m = mask(size);
    let (a, b) = (a & m, b & m);
    let r = if sub { a.wrapping_sub(b) } else { a.wrapping_add(b) } & m;
    cpu.flags.cf = if sub { a < b } else { (a as u128 + b as u128) > m as u128 };
    cpu.flags.of = if sub { sign(a, size) != sign(b, size) && sign(r, size) != sign(a, size) } else { sign(a, size) == sign(b, size) && sign(r, size) != sign(a, size) };
    cpu.set_szp(r, size);
    r
}

fn logic(cpu: &mut Cpu, r: u64, size: usize) -> u64 {
    cpu.flags.cf = false;
    cpu.flags.of = false;
    cpu.set_szp(r, size);
    r & mask(size)
}

/// 整数の命令を実行する。扱わない命令なら Ok(false)（SSE などほかへ回す）
pub fn exec(cpu: &mut Cpu, mem: &mut Memory, i: &Instruction) -> R<bool> {
    let m = i.mnemonic();
    match m {
        M::Nop | M::Endbr64 => {}
        M::Int3 => return Err(Fault("int3".into())),
        M::Mov => {
            let (v, _) = cpu.read_op(mem, i, 1)?;
            cpu.write_op(mem, i, 0, v)?;
        }
        M::Movzx => {
            let (v, s) = cpu.read_op(mem, i, 1)?;
            cpu.write_op(mem, i, 0, v & mask(s))?;
        }
        M::Movsx | M::Movsxd => {
            let (v, s) = cpu.read_op(mem, i, 1)?;
            cpu.write_op(mem, i, 0, sext(v, s))?;
        }
        M::Lea => {
            let a = cpu.addr(i)?;
            cpu.write_op(mem, i, 0, a)?;
        }
        M::Add | M::Sub | M::Cmp | M::Adc | M::Sbb => {
            let size = dst_size(i);
            let (a, _) = cpu.read_op(mem, i, 0)?;
            let (b, _) = cpu.read_op(mem, i, 1)?;
            let carry = matches!(m, M::Adc | M::Sbb) && cpu.flags.cf;
            let b = b.wrapping_add(carry as u64);
            let r = add_sub(cpu, a, b, size, matches!(m, M::Sub | M::Cmp | M::Sbb));
            if m != M::Cmp {
                cpu.write_op(mem, i, 0, r)?;
            }
        }
        M::And | M::Or | M::Xor | M::Test => {
            let size = dst_size(i);
            let (a, _) = cpu.read_op(mem, i, 0)?;
            let (b, _) = cpu.read_op(mem, i, 1)?;
            let r = match m {
                M::Or => a | b,
                M::Xor => a ^ b,
                _ => a & b,
            };
            let r = logic(cpu, r, size);
            if m != M::Test {
                cpu.write_op(mem, i, 0, r)?;
            }
        }
        M::Inc | M::Dec => {
            let size = dst_size(i);
            let (a, _) = cpu.read_op(mem, i, 0)?;
            let cf = cpu.flags.cf;
            let r = add_sub(cpu, a, 1, size, m == M::Dec);
            cpu.flags.cf = cf;
            cpu.write_op(mem, i, 0, r)?;
        }
        M::Neg => {
            let size = dst_size(i);
            let (a, _) = cpu.read_op(mem, i, 0)?;
            let r = add_sub(cpu, 0, a, size, true);
            cpu.write_op(mem, i, 0, r)?;
        }
        M::Not => {
            let (a, _) = cpu.read_op(mem, i, 0)?;
            cpu.write_op(mem, i, 0, !a)?;
        }
        M::Shl | M::Shr | M::Sar => {
            let size = dst_size(i);
            let (a, _) = cpu.read_op(mem, i, 0)?;
            let c = if i.op_count() > 1 { cpu.read_op(mem, i, 1)?.0 } else { 1 } & if size == 8 { 63 } else { 31 };
            if c != 0 {
                let a = a & mask(size);
                let r = match m {
                    M::Shl => a << c,
                    M::Shr => a >> c,
                    _ => sext(a, size) >> c | 0,
                };
                let r = if m == M::Sar { ((sext(a, size) as i64) >> c) as u64 } else { r };
                cpu.flags.cf = match m {
                    M::Shl => (a >> (size as u64 * 8 - c)) & 1 == 1,
                    _ => (sext(a, size) >> (c - 1)) & 1 == 1,
                };
                cpu.set_szp(r, size);
                cpu.write_op(mem, i, 0, r & mask(size))?;
            }
        }
        M::Imul if i.op_count() >= 2 => {
            let size = dst_size(i);
            let (a, b) = if i.op_count() == 3 { (cpu.read_op(mem, i, 1)?.0, cpu.read_op(mem, i, 2)?.0) } else { (cpu.read_op(mem, i, 0)?.0, cpu.read_op(mem, i, 1)?.0) };
            let full = (sext(a, size) as i64 as i128) * (sext(b, size) as i64 as i128);
            let r = full as u64 & mask(size);
            let over = sext(r, size) as i64 as i128 != full;
            cpu.flags.cf = over;
            cpu.flags.of = over;
            cpu.write_op(mem, i, 0, r)?;
        }
        M::Cdqe => cpu.regs[0] = sext(cpu.regs[0], 4),
        M::Cdq => cpu.regs[2] = if sign(cpu.regs[0], 4) { 0xffff_ffff } else { 0 },
        M::Cqo => cpu.regs[2] = if sign(cpu.regs[0], 8) { u64::MAX } else { 0 },
        M::Xchg => {
            let (a, _) = cpu.read_op(mem, i, 0)?;
            let (b, _) = cpu.read_op(mem, i, 1)?;
            cpu.write_op(mem, i, 0, b)?;
            cpu.write_op(mem, i, 1, a)?;
        }
        M::Push => {
            let (v, _) = cpu.read_op(mem, i, 0)?;
            cpu.push(mem, v)?;
        }
        M::Pop => {
            let v = cpu.pop(mem)?;
            cpu.write_op(mem, i, 0, v)?;
        }
        // 文字列のコピー（movs）と書き込み（stos）。rep が付けば rcx 回繰り返す。方向フラグ（df）は 0 として扱う（前へ進む）
        M::Movsb | M::Movsw | M::Movsq => string_movs_stos(cpu, mem, i, true)?,
        // movs dword（文字列）だけは SSE の movsd と名前が重なるので Code で見分ける
        M::Movsd if i.code() == Code::Movsd_m32_m32 => string_movs_stos(cpu, mem, i, true)?,
        M::Stosb | M::Stosw | M::Stosd | M::Stosq => string_movs_stos(cpu, mem, i, false)?,
        _ => return exec_rest(cpu, mem, i),
    }
    Ok(true)
}

/// movs（コピー）と stos（書き込み）。rep が付けば rcx 回。方向フラグは 0（前へ進む）として扱う
fn string_movs_stos(cpu: &mut Cpu, mem: &mut Memory, i: &Instruction, movs: bool) -> R<()> {
    let m = i.mnemonic();
    let size = match m {
        M::Movsb | M::Stosb => 1,
        M::Movsw | M::Stosw => 2,
        M::Movsq | M::Stosq => 8,
        _ => 4,
    };
    let count = if i.has_rep_prefix() { cpu.regs[1] } else { 1 };
    for _ in 0..count {
        let v = if movs { mem.read_n(cpu.regs[6], size)? } else { cpu.regs[0] & mask(size) };
        mem.write_n(cpu.regs[7], size, v)?; // rdi
        if movs {
            cpu.regs[6] = cpu.regs[6].wrapping_add(size as u64); // rsi
        }
        cpu.regs[7] = cpu.regs[7].wrapping_add(size as u64);
    }
    if i.has_rep_prefix() {
        cpu.regs[1] = 0;
    }
    Ok(())
}

/// 残りの命令（分岐、条件つき）。exec の match の続き
fn exec_rest(cpu: &mut Cpu, mem: &mut Memory, i: &Instruction) -> R<bool> {
    let m = i.mnemonic();
    match m {
        M::Leave => {
            cpu.regs[RSP] = cpu.regs[5];
            cpu.regs[5] = cpu.pop(mem)?;
        }
        M::Call => {
            let target = branch_target(cpu, mem, i)?;
            cpu.push(mem, i.next_ip())?;
            cpu.rip = target;
        }
        M::Jmp => cpu.rip = branch_target(cpu, mem, i)?,
        M::Ret => {
            cpu.rip = cpu.pop(mem)?;
            if i.op_count() == 1 {
                cpu.regs[RSP] = cpu.regs[RSP].wrapping_add(i.immediate(0));
            }
        }
        // 条件つきの命令（jcc、setcc、cmovcc）。引数の形で見分ける
        _ if i.condition_code() != ConditionCode::None => {
            let take = cpu.cond(i.condition_code());
            match i.op_kind(0) {
                // jcc（飛び先が分岐の番地）
                OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                    if take {
                        cpu.rip = i.near_branch_target();
                    }
                }
                // cmovcc（引数が 2 つ）か setcc（引数が 1 つ）
                _ if i.op_count() == 2 => {
                    let (v, _) = cpu.read_op(mem, i, 1)?;
                    if take {
                        cpu.write_op(mem, i, 0, v)?;
                    }
                }
                _ => cpu.write_op(mem, i, 0, take as u64)?,
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn branch_target(cpu: &Cpu, mem: &Memory, i: &Instruction) -> R<u64> {
    match i.op_kind(0) {
        OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => Ok(i.near_branch_target()),
        _ => Ok(cpu.read_op(mem, i, 0)?.0),
    }
}
