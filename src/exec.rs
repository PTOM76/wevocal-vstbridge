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

/// 足し算と引き算（cmp も）。結果とフラグ。carry は adc、sbb のときの繰り上がり（0 か 1）
fn add_sub(cpu: &mut Cpu, a: u64, b: u64, size: usize, sub: bool, carry: u64) -> u64 {
    let m = mask(size);
    let (a, b) = (a & m, b & m);
    // 繰り上がりは b に足さず別に扱う（b が全部 1 のとき、足して 0 になるとキャリーが消えるため）
    let r = if sub { a.wrapping_sub(b).wrapping_sub(carry) } else { a.wrapping_add(b).wrapping_add(carry) } & m;
    cpu.flags.cf = if sub { (a as u128) < (b as u128 + carry as u128) } else { (a as u128 + b as u128 + carry as u128) > m as u128 };
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
            let carry = (matches!(m, M::Adc | M::Sbb) && cpu.flags.cf) as u64;
            let r = add_sub(cpu, a, b, size, matches!(m, M::Sub | M::Cmp | M::Sbb), carry);
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
        M::Bsf | M::Bsr => {
            // 立っている一番下（bsf）、一番上（bsr）のビットの位置。0 なら ZF を立て、行き先は変えない
            let size = dst_size(i);
            let (v, _) = cpu.read_op(mem, i, 1)?;
            let v = v & mask(size);
            cpu.flags.zf = v == 0;
            if v != 0 {
                let n = if m == M::Bsf { v.trailing_zeros() } else { 63 - v.leading_zeros() };
                cpu.write_op(mem, i, 0, n as u64)?;
            }
        }
        M::Bt | M::Bts | M::Btr | M::Btc => {
            // ビットを調べて CF に入れる（bts、btr、btc は、そのビットを立てる、消す、反転する）。番号は大きさで回り込む
            let size = dst_size(i);
            let (a, _) = cpu.read_op(mem, i, 0)?;
            let (b, _) = cpu.read_op(mem, i, 1)?;
            let bit = 1u64 << (b % (size as u64 * 8));
            cpu.flags.cf = a & bit != 0;
            let r = match m {
                M::Bts => a | bit,
                M::Btr => a & !bit,
                M::Btc => a ^ bit,
                _ => return Ok(true),
            };
            cpu.write_op(mem, i, 0, r)?;
        }
        M::Inc | M::Dec => {
            let size = dst_size(i);
            let (a, _) = cpu.read_op(mem, i, 0)?;
            let cf = cpu.flags.cf;
            let r = add_sub(cpu, a, 1, size, m == M::Dec, 0);
            cpu.flags.cf = cf;
            cpu.write_op(mem, i, 0, r)?;
        }
        M::Neg => {
            let size = dst_size(i);
            let (a, _) = cpu.read_op(mem, i, 0)?;
            let r = add_sub(cpu, 0, a, size, true, 0);
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
                    _ => ((sext(a, size) as i64) >> c) as u64,
                };
                // 最後に押し出されたビット
                cpu.flags.cf = match m {
                    M::Shl => (a >> (size as u64 * 8 - c)) & 1 == 1,
                    M::Shr => (a >> (c - 1)) & 1 == 1,
                    _ => ((sext(a, size) as i64) >> (c - 1)) & 1 == 1,
                };
                cpu.set_szp(r, size);
                cpu.write_op(mem, i, 0, r & mask(size))?;
            }
        }
        // 回転（rol、ror）。キャリーは最後に回り込んだビット
        M::Rol | M::Ror => {
            let size = dst_size(i);
            let bits = size as u64 * 8;
            let (a, _) = cpu.read_op(mem, i, 0)?;
            let c = (if i.op_count() > 1 { cpu.read_op(mem, i, 1)?.0 } else { 1 } & if size == 8 { 63 } else { 31 }) % bits;
            let a = a & mask(size);
            if c != 0 {
                let r = if m == M::Rol { (a << c) | (a >> (bits - c)) } else { (a >> c) | (a << (bits - c)) } & mask(size);
                cpu.flags.cf = if m == M::Rol { r & 1 == 1 } else { sign(r, size) };
                cpu.write_op(mem, i, 0, r)?;
            }
        }
        // ecx（32 ビットでは ecx）を 1 減らし、0 でなければ分岐する（フラグは変えない）
        M::Loop | M::Loope | M::Loopne => {
            let mut ecx = cpu.regs[1] & mask(if cpu.width == 4 { 4 } else { 8 });
            ecx = ecx.wrapping_sub(1) & mask(if cpu.width == 4 { 4 } else { 8 });
            cpu.regs[1] = ecx;
            let zf = cpu.flags.zf;
            let go = ecx != 0 && match m { M::Loope => zf, M::Loopne => !zf, _ => true };
            if go {
                cpu.rip = i.near_branch_target();
            }
        }
        M::Jecxz | M::Jrcxz => {
            if cpu.regs[1] & mask(if cpu.width == 4 { 4 } else { 8 }) == 0 {
                cpu.rip = i.near_branch_target();
            }
        }
        M::Bswap => {
            let (a, size) = cpu.read_op(mem, i, 0)?;
            let r = if size == 8 { a.swap_bytes() } else { (a as u32).swap_bytes() as u64 };
            cpu.write_op(mem, i, 0, r)?;
        }
        // 1 引数の乗除算。幅ごとに、積は (上位:下位) の組（8 ビットは ax、16 ビットは dx:ax、32 ビットは edx:eax、64 ビットは rdx:rax）に入る
        M::Mul | M::Imul | M::Div | M::Idiv if i.op_count() == 1 => {
            let size = dst_size(i);
            let (v, _) = cpu.read_op(mem, i, 0)?;
            let v = v & mask(size);
            let signed = matches!(m, M::Imul | M::Idiv);
            let bits = size * 8;
            // 符号つきなら符号を広げた i128、なければそのままの u128 で計算する
            let wide = |x: u64| -> i128 { if signed { sext(x, size) as i64 as i128 } else { (x & mask(size)) as i128 } };
            let acc_lo = if size == 1 { cpu.regs[0] & 0xff } else { cpu.regs[0] & mask(size) };
            let hi = if size == 1 { (cpu.regs[0] >> 8) & 0xff } else { cpu.regs[2] & mask(size) };
            if matches!(m, M::Mul | M::Imul) {
                let prod = wide(acc_lo) * wide(v);
                let lo = (prod as u128 & ((1u128 << bits) - 1)) as u64;
                let up = ((prod >> bits) as u128 & ((1u128 << bits) - 1)) as u64;
                if size == 1 {
                    cpu.regs[0] = (cpu.regs[0] & !0xffff) | (up << 8) | lo;
                } else {
                    cpu.set_reg(match size { 2 => iced_x86::Register::AX, 4 => iced_x86::Register::EAX, _ => iced_x86::Register::RAX }, lo)?;
                    cpu.set_reg(match size { 2 => iced_x86::Register::DX, 4 => iced_x86::Register::EDX, _ => iced_x86::Register::RDX }, up)?;
                }
                // 上位が下位の符号拡張（imul）、または 0（mul）でなければ、桁あふれ
                let over = if signed { prod != sext(lo, size) as i64 as i128 } else { up != 0 };
                cpu.flags.cf = over;
                cpu.flags.of = over;
            } else {
                if v == 0 {
                    return Err(Fault("divide by zero".into()));
                }
                let dividend: i128 = if signed {
                    let top = (hi as i128) << bits;
                    let full = top | acc_lo as i128;
                    // 上位を含めた全体を符号つきの数にする
                    (full << (128 - 2 * bits)) >> (128 - 2 * bits)
                } else {
                    ((hi as u128) << bits | acc_lo as u128) as i128
                };
                let d = wide(v);
                let (q, r) = (dividend / d, dividend % d);
                if size == 1 {
                    cpu.regs[0] = (cpu.regs[0] & !0xffff) | (((r as u64) & 0xff) << 8) | ((q as u64) & 0xff);
                } else {
                    cpu.set_reg(match size { 2 => iced_x86::Register::AX, 4 => iced_x86::Register::EAX, _ => iced_x86::Register::RAX }, q as u64)?;
                    cpu.set_reg(match size { 2 => iced_x86::Register::DX, 4 => iced_x86::Register::EDX, _ => iced_x86::Register::RDX }, r as u64)?;
                }
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
        // ah の下位 8 ビットをフラグに移す（sf、zf、af、pf、cf。bit1 は常に 1）。浮動小数点の比較の結果を使うときに出る
        M::Sahf => {
            let ah = (cpu.regs[0] >> 8) & 0xff;
            cpu.flags.cf = ah & 1 != 0;
            cpu.flags.pf = ah & 4 != 0;
            cpu.flags.zf = ah & 0x40 != 0;
            cpu.flags.sf = ah & 0x80 != 0;
        }
        // フラグを ah に集める（sahf の逆）
        M::Lahf => {
            let f = cpu.flags;
            let ah = 2 | (f.cf as u64) | (f.pf as u64) << 2 | (f.zf as u64) << 6 | (f.sf as u64) << 7;
            cpu.set_reg(iced_x86::Register::AH, ah)?;
        }
        M::Cdqe => cpu.regs[0] = sext(cpu.regs[0], 4),
        // ax → eax（上位 32 ビットは 0）、al → ax（上位はそのまま）
        M::Cwde => cpu.regs[0] = sext(cpu.regs[0], 2) & 0xffff_ffff,
        M::Cbw => cpu.regs[0] = (cpu.regs[0] & !0xffff) | (sext(cpu.regs[0], 1) & 0xffff),
        M::Cwd => cpu.regs[2] = (cpu.regs[2] & !0xffff) | if sign(cpu.regs[0], 2) { 0xffff } else { 0 },
        M::Cdq => cpu.regs[2] = if sign(cpu.regs[0], 4) { 0xffff_ffff } else { 0 },
        M::Cqo => cpu.regs[2] = if sign(cpu.regs[0], 8) { u64::MAX } else { 0 },
        M::Xchg => {
            let (a, _) = cpu.read_op(mem, i, 0)?;
            let (b, _) = cpu.read_op(mem, i, 1)?;
            cpu.write_op(mem, i, 0, b)?;
            cpu.write_op(mem, i, 1, a)?;
        }
        // スレッドは 1 つなので lock は無視してよい
        M::Xadd => {
            let size = dst_size(i);
            let (a, _) = cpu.read_op(mem, i, 0)?;
            let (b, _) = cpu.read_op(mem, i, 1)?;
            let r = add_sub(cpu, a, b, size, false, 0);
            cpu.write_op(mem, i, 1, a)?;
            cpu.write_op(mem, i, 0, r)?;
        }
        M::Cmpxchg => {
            // 引数 0 と rax を比べ、同じなら引数 1 を書き、違えば rax に読む
            let size = dst_size(i);
            let (a, _) = cpu.read_op(mem, i, 0)?;
            let (b, _) = cpu.read_op(mem, i, 1)?;
            let acc = cpu.regs[0] & mask(size);
            add_sub(cpu, acc, a, size, true, 0);
            if acc == a & mask(size) {
                cpu.write_op(mem, i, 0, b)?;
            } else if size == 4 {
                cpu.regs[0] = a & mask(4);
            } else {
                cpu.regs[0] = (cpu.regs[0] & !mask(size)) | (a & mask(size));
            }
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
        M::Scasb | M::Scasw | M::Scasd | M::Scasq => string_scas(cpu, mem, i)?,
        M::Lodsb | M::Lodsw | M::Lodsd | M::Lodsq => {
            let size = match m { M::Lodsb => 1, M::Lodsw => 2, M::Lodsq => 8, _ => 4 };
            let count = if i.has_rep_prefix() { cpu.regs[1] } else { 1 };
            for _ in 0..count {
                let v = mem.read_n(cpu.regs[6], size)?;
                cpu.set_reg(if size == 1 { iced_x86::Register::AL } else if size == 2 { iced_x86::Register::AX } else if size == 4 { iced_x86::Register::EAX } else { iced_x86::Register::RAX }, v)?;
                cpu.regs[6] = cpu.regs[6].wrapping_add(size as u64);
            }
            if i.has_rep_prefix() { cpu.regs[1] = 0; }
        }
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

/// scas（al/ax/eax と [edi] を比べる）。repe は等しい間、repne は等しくなるまで、ecx 回まで繰り返す。方向フラグは 0（前へ進む）
fn string_scas(cpu: &mut Cpu, mem: &mut Memory, i: &Instruction) -> R<()> {
    let size = match i.mnemonic() {
        M::Scasb => 1,
        M::Scasw => 2,
        M::Scasq => 8,
        _ => 4,
    };
    let acc = cpu.regs[0] & mask(size);
    let repe = i.has_repe_prefix();
    let repne = i.has_repne_prefix();
    let rep = repe || repne;
    let mut count = if rep { cpu.regs[1] } else { 1 };
    while count > 0 {
        let v = mem.read_n(cpu.regs[7], size)?;
        add_sub(cpu, acc, v, size, true, 0); // cmp と同じにフラグを決める
        cpu.regs[7] = cpu.regs[7].wrapping_add(size as u64);
        count -= 1;
        if rep {
            cpu.regs[1] = count;
            // repe は等しい間（zf=1）続け、repne は等しくなる（zf=1）まで続ける
            if (repe && !cpu.flags.zf) || (repne && cpu.flags.zf) {
                break;
            }
        }
    }
    Ok(())
}

/// 残りの命令（分岐、条件つき）。exec の match の続き
fn exec_rest(cpu: &mut Cpu, mem: &mut Memory, i: &Instruction) -> R<bool> {
    let m = i.mnemonic();
    match m {
        // pushad/popad（32 ビットだけ。eax、ecx、edx、ebx、元の esp、ebp、esi、edi の順に積む）
        M::Pushad => {
            let order = [0usize, 1, 2, 3, RSP, 5, 6, 7];
            let esp = cpu.regs[RSP] & mask(4);
            for (i, &r) in order.iter().enumerate() {
                let v = if r == RSP { esp } else { cpu.regs[r] };
                cpu.push(mem, v)?;
                let _ = i;
            }
        }
        M::Popad => {
            // esp は popad では書き換えない決まり（ポップはするが捨てる）
            for &r in [7usize, 6, 5, RSP, 3, 2, 1, 0].iter() {
                let v = cpu.pop(mem)?;
                if r != RSP {
                    cpu.regs[r] = v & mask(4);
                }
            }
        }
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
