//! push、pop と、ブロックの終わりの call、ret、間接の jmp を wasm に変換する（インタープリターの exec.rs と同じ動き）。
//! 中断（メモリの遅い道）してもやり直せるよう、rsp はメモリの番地を引けたあとで変える
use super::*;
use iced_x86::CodeSize;

/// push、pop、call、ret が動かす幅（32 ビットのコードは 4、64 ビットは 8）
fn width(i: &Instruction) -> usize {
    if i.code_size() == CodeSize::Code64 { 8 } else { 4 }
}

/// 引数 0 が、幅と同じ大きさのレジスタかメモリか、即値か
fn op0_ok(i: &Instruction) -> bool {
    let w = width(i);
    match i.op_kind(0) {
        OpKind::Register => i.op_register(0).size() == w && gpr(i.op_register(0)).is_some(),
        OpKind::Memory => i.memory_size().size() == w && operand(i, 0).is_some(),
        _ => operand(i, 0).is_some(),
    }
}

/// push、pop を変換できるか（pop はレジスタへだけ。メモリへの pop は rsp を変えたあとで中断しうるため）
pub(super) fn rw(i: &Instruction) -> Option<Rw> {
    let ok = match i.mnemonic() {
        M::Push => i.op_count() == 1 && op0_ok(i),
        M::Pop => i.op_count() == 1 && i.op_kind(0) == OpKind::Register && op0_ok(i),
        _ => false,
    };
    ok.then_some(Rw { kill: 0, emit: 0, read: 0 })
}

/// ブロックの終わりにできるか（call、ret、間接の jmp）
pub(super) fn term_ok(i: &Instruction) -> bool {
    match i.flow_control() {
        FlowControl::Call | FlowControl::IndirectCall => i.op_count() == 1 && (matches!(i.op_kind(0), OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64) || (!matches!(i.op_kind(0), OpKind::FarBranch16 | OpKind::FarBranch32) && op0_ok(i))),
        FlowControl::Return => i.mnemonic() == M::Ret && (i.op_count() == 0 || i.op_kind(0) == OpKind::Immediate16),
        FlowControl::IndirectBranch => i.mnemonic() == M::Jmp && i.op_count() == 1 && op0_ok(i),
        _ => false,
    }
}

impl Asm {
    /// rsp の 64 ビットをそのまま書く（インタープリターの wrapping_add、wrapping_sub と同じ）
    fn set_rsp_from(&mut self, local: u32) {
        self.cpu();
        self.body.push(W::LocalGet(local));
        self.body.push(W::I64Store(MemArg { offset: off_reg(4), align: 3, memory_index: 0 }));
    }
    /// B の値を積む（rsp - w に書いて rsp を下げる）。番地が引けなければ、何も変えずに中断
    fn push_b(&mut self, w: usize, insn_addr: u64, count: u32) {
        self.load_reg(4);
        self.body.push(W::I64Const(w as i64));
        self.body.push(W::I64Sub);
        self.body.push(W::LocalTee(A));
        self.body.push(W::I32WrapI64);
        self.body.push(W::LocalSet(GA));
        self.resolve_ptr_w(w, insn_addr, count);
        self.body.push(W::LocalGet(HP));
        self.body.push(W::LocalGet(B));
        self.body.push(store_op(w));
        self.set_rsp_from(A);
    }
    /// rsp から w バイトを B に読み、rsp を w + extra だけ上げる
    fn pop_b(&mut self, w: usize, extra: u64, insn_addr: u64, count: u32) {
        self.load_reg(4);
        self.body.push(W::LocalTee(A));
        self.body.push(W::I32WrapI64);
        self.body.push(W::LocalSet(GA));
        self.resolve_ptr(w, insn_addr, count);
        self.body.push(W::LocalGet(HP));
        self.body.push(load_op(w));
        self.body.push(W::LocalSet(B));
        self.body.push(W::LocalGet(A));
        self.body.push(W::I64Const((w as u64).wrapping_add(extra) as i64));
        self.body.push(W::I64Add);
        self.body.push(W::LocalSet(A));
        self.set_rsp_from(A);
    }
    /// rip を局所 local の値にして、count を返す
    fn emit_exit_dyn(&mut self, local: u32, count: u32) {
        self.fpu_sync();
        self.cpu();
        self.body.push(W::LocalGet(local));
        self.body.push(W::I64Store(MemArg { offset: self.off.rip, align: 3, memory_index: 0 }));
        self.ret_count(count);
        self.body.push(W::Return);
    }
}

/// push、pop
pub(super) fn emit(asm: &mut Asm, i: &Instruction, insn_addr: u64, count: u32) -> Option<()> {
    let w = width(i);
    match i.mnemonic() {
        M::Push => {
            asm.push_operand(&operand(i, 0)?, insn_addr, count);
            asm.body.push(W::LocalSet(B));
            asm.push_b(w, insn_addr, count);
        }
        _ => {
            let (idx, _) = gpr(i.op_register(0))?;
            asm.pop_b(w, 0, insn_addr, count);
            asm.body.push(W::LocalGet(B));
            asm.store_reg(idx, w);
        }
    }
    Some(())
}

/// ブロックの終わりの call、ret、間接の jmp。count はこの命令を含めた数（中断のときは含めない）
pub(super) fn emit_term(asm: &mut Asm, i: &Instruction, count: u32) {
    let (w, at, before) = (width(i), i.ip(), count - 1);
    match i.flow_control() {
        FlowControl::Return => {
            let extra = if i.op_count() == 1 { i.immediate(0) } else { 0 };
            asm.pop_b(w, extra, at, before);
            asm.emit_exit_dyn(B, count);
        }
        flow => {
            // 飛び先を R に（間接なら引数を読む。読めなければ中断）
            if matches!(i.op_kind(0), OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64) {
                asm.body.push(W::I64Const(i.near_branch_target() as i64));
            } else if let Some(op) = operand(i, 0) {
                asm.push_operand(&op, at, before);
            }
            asm.body.push(W::LocalSet(R));
            if flow != FlowControl::IndirectBranch {
                asm.body.push(W::I64Const(i.next_ip() as i64));
                asm.body.push(W::LocalSet(B));
                asm.push_b(w, at, before);
            }
            asm.emit_exit_dyn(R, count);
        }
    }
}
