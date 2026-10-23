//! 整数の命令の残り（cdq、cqo、div、idiv、引数が 1 つの mul、imul）を wasm に変換する（インタープリターの exec.rs と同じ動き）
use super::*;

/// 変換できるか。mul、imul は 32 と 64 ビットの引数だけ
pub(super) fn rw(i: &Instruction) -> Option<Rw> {
    let ok = match i.mnemonic() {
        M::Cdq | M::Cqo => true,
        M::Div | M::Idiv => i.op_count() == 1 && int_operands_ok(i) && matches!(dst_size(i), 4 | 8),
        // cf と of だけを書く
        M::Mul | M::Imul if i.op_count() == 1 && int_operands_ok(i) && matches!(dst_size(i), 4 | 8) => return Some(Rw { kill: FCF | FOF, emit: FCF | FOF, read: 0 }),
        _ => false,
    };
    ok.then_some(Rw { kill: 0, emit: 0, read: 0 })
}

pub(super) fn emit(asm: &mut Asm, i: &Instruction, insn_addr: u64, count: u32) -> Option<()> {
    match i.mnemonic() {
        // edx（rdx）を eax（rax）の符号で埋める
        M::Cdq | M::Cqo => {
            let (size, shift) = if i.mnemonic() == M::Cdq { (4, 31) } else { (8, 63) };
            asm.load_reg(0);
            if size == 4 {
                asm.body.push(W::I64Extend32S);
            }
            asm.body.extend([W::I64Const(shift), W::I64ShrS]);
            asm.store_reg(2, size);
        }
        // edx:eax を割り、商を eax、余りを edx に。0 で割るときと、符号つきの -1 で割るとき（wasm が止まる形）はインタープリターに任せる
        M::Div | M::Idiv if dst_size(i) == 8 => {
            // 割られる数が rax に収まる（rdx が rax の符号か 0）ときだけ。ほかと、0 と -1 で割るときはインタープリター
            let signed = i.mnemonic() == M::Idiv;
            asm.push_operand(&operand(i, 0)?, insn_addr, count);
            asm.body.push(W::LocalTee(B));
            asm.body.push(W::I64Eqz);
            if signed {
                asm.body.extend([W::LocalGet(B), W::I64Const(-1), W::I64Eq, W::I32Or]);
            }
            asm.load_reg(2);
            if signed {
                asm.load_reg(0);
                asm.body.extend([W::I64Const(63), W::I64ShrS, W::I64Ne]);
            } else {
                asm.body.extend([W::I64Const(0), W::I64Ne]);
            }
            asm.body.push(W::I32Or);
            asm.body.push(W::If(BlockType::Empty));
            asm.emit_bail(insn_addr, count);
            asm.body.push(W::End);
            let (div, rem) = if signed { (W::I64DivS, W::I64RemS) } else { (W::I64DivU, W::I64RemU) };
            // 割られる数を A に取ってから、余り（rdx）、商（rax）の順に書く
            asm.load_reg(0);
            asm.body.extend([W::LocalSet(A), W::LocalGet(A), W::LocalGet(B), rem]);
            asm.store_reg(2, 8);
            asm.body.extend([W::LocalGet(A), W::LocalGet(B), div]);
            asm.store_reg(0, 8);
        }
        // edx:eax = eax × 引数。上位が下位の符号拡張（imul）か 0（mul）でなければ cf、of を立てる
        // rdx:rax = rax × 引数。上位は 32 ビットずつに分けて求める（符号つきは、負の側の分を引いて直す）
        M::Mul | M::Imul if dst_size(i) == 8 => {
            let signed = i.mnemonic() == M::Imul;
            asm.push_operand(&operand(i, 0)?, insn_addr, count);
            asm.body.push(W::LocalSet(B));
            asm.load_reg(0);
            asm.body.push(W::LocalSet(A));
            let lo = |x: u32| [W::LocalGet(x), W::I64Const(0xffff_ffff), W::I64And];
            let hi = |x: u32| [W::LocalGet(x), W::I64Const(32), W::I64ShrU];
            // X0 = 上位 a × 下位 b + (下位 a × 下位 b) >> 32
            asm.body.extend(hi(A));
            asm.body.extend(lo(B));
            asm.body.push(W::I64Mul);
            asm.body.extend(lo(A));
            asm.body.extend(lo(B));
            asm.body.extend([W::I64Mul, W::I64Const(32), W::I64ShrU, W::I64Add, W::LocalSet(X0)]);
            // X1 = 下位 a × 上位 b + 下位 X0
            asm.body.extend(lo(A));
            asm.body.extend(hi(B));
            asm.body.push(W::I64Mul);
            asm.body.extend(lo(X0));
            asm.body.extend([W::I64Add, W::LocalSet(X1)]);
            // X0 = 上位 a × 上位 b + 上位 X0 + 上位 X1（積の上位 64 ビット）
            asm.body.extend(hi(A));
            asm.body.extend(hi(B));
            asm.body.push(W::I64Mul);
            asm.body.extend(hi(X0));
            asm.body.push(W::I64Add);
            asm.body.extend(hi(X1));
            asm.body.extend([W::I64Add, W::LocalSet(X0)]);
            if signed {
                // a < 0 なら b を、b < 0 なら a を引く
                for (x, y) in [(A, B), (B, A)] {
                    asm.body.extend([W::LocalGet(X0), W::LocalGet(y), W::LocalGet(x), W::I64Const(63), W::I64ShrS, W::I64And, W::I64Sub, W::LocalSet(X0)]);
                }
            }
            asm.body.extend([W::LocalGet(A), W::LocalGet(B), W::I64Mul, W::LocalSet(X1)]);
            if asm.flag_mask & (FCF | FOF) != 0 {
                asm.body.push(W::LocalGet(X0));
                if signed {
                    asm.body.extend([W::LocalGet(X1), W::I64Const(63), W::I64ShrS, W::I64Ne]);
                } else {
                    asm.body.extend([W::I64Const(0), W::I64Ne]);
                }
                asm.body.push(W::LocalSet(GA));
                for (bit, off) in [(FCF, asm.off.cf), (FOF, asm.off.of)] {
                    if asm.flag_mask & bit != 0 {
                        asm.cpu();
                        asm.body.extend([W::LocalGet(GA), W::I32Store8(MemArg { offset: off, align: 0, memory_index: 0 })]);
                    }
                }
            }
            asm.body.push(W::LocalGet(X1));
            asm.store_reg(0, 8);
            asm.body.push(W::LocalGet(X0));
            asm.store_reg(2, 8);
        }
        M::Mul | M::Imul => {
            let signed = i.mnemonic() == M::Imul;
            asm.push_operand(&operand(i, 0)?, insn_addr, count);
            if signed {
                asm.body.push(W::I64Extend32S);
            }
            asm.load_reg(0);
            asm.body.extend([W::I64Const(0xffff_ffff), W::I64And]);
            if signed {
                asm.body.push(W::I64Extend32S);
            }
            asm.body.extend([W::I64Mul, W::LocalSet(A)]);
            if asm.flag_mask & (FCF | FOF) != 0 {
                asm.body.push(W::LocalGet(A));
                if signed {
                    asm.body.extend([W::LocalGet(A), W::I64Extend32S, W::I64Ne]);
                } else {
                    asm.body.extend([W::I64Const(32), W::I64ShrU, W::I64Const(0), W::I64Ne]);
                }
                asm.body.push(W::LocalSet(GA));
                for (bit, off) in [(FCF, asm.off.cf), (FOF, asm.off.of)] {
                    if asm.flag_mask & bit != 0 {
                        asm.cpu();
                        asm.body.extend([W::LocalGet(GA), W::I32Store8(MemArg { offset: off, align: 0, memory_index: 0 })]);
                    }
                }
            }
            asm.body.push(W::LocalGet(A));
            asm.store_reg(0, 4);
            asm.body.extend([W::LocalGet(A), W::I64Const(32), W::I64ShrU]);
            asm.store_reg(2, 4);
        }
        M::Div | M::Idiv => {
            let signed = i.mnemonic() == M::Idiv;
            asm.push_operand(&operand(i, 0)?, insn_addr, count);
            if signed {
                asm.body.push(W::I64Extend32S);
            }
            asm.body.push(W::LocalTee(B));
            asm.body.push(W::I64Eqz);
            if signed {
                asm.body.extend([W::LocalGet(B), W::I64Const(-1), W::I64Eq, W::I32Or]);
            }
            asm.body.push(W::If(BlockType::Empty));
            asm.emit_bail(insn_addr, count);
            asm.body.push(W::End);
            // A = edx << 32 | eax
            asm.load_reg(2);
            asm.body.extend([W::I64Const(32), W::I64Shl]);
            asm.load_reg(0);
            asm.body.extend([W::I64Const(0xffff_ffff), W::I64And, W::I64Or, W::LocalSet(A)]);
            let (div, rem) = if signed { (W::I64DivS, W::I64RemS) } else { (W::I64DivU, W::I64RemU) };
            asm.body.extend([W::LocalGet(A), W::LocalGet(B), rem]);
            asm.body.push(W::LocalSet(R));
            asm.body.extend([W::LocalGet(A), W::LocalGet(B), div]);
            // store_reg は R を使うので、余りを A に移してから商を書く
            asm.body.extend([W::LocalGet(R), W::LocalSet(A)]);
            asm.store_reg(0, 4);
            asm.body.push(W::LocalGet(A));
            asm.store_reg(2, 4);
        }
        _ => return None,
    }
    Some(())
}
