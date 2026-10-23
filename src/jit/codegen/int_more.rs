//! 整数の命令の残り（cdq、cqo、32 ビットの div、idiv）を wasm に変換する（インタープリターの exec.rs と同じ動き。フラグは変えない）
use super::*;

/// 変換できるか。div、idiv は 32 ビットの引数だけ
pub(super) fn rw(i: &Instruction) -> Option<Rw> {
    let ok = match i.mnemonic() {
        M::Cdq | M::Cqo => true,
        M::Div | M::Idiv => i.op_count() == 1 && int_operands_ok(i) && dst_size(i) == 4,
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
