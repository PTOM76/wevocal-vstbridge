//! 分岐でつながる複数の基本ブロックを 1 つの wasm の関数にまとめる。ブロックの間は局所 PC と br_table で移り、Rust に戻らない。
//! 直接の call は、呼ぶ先と戻り先もまとめに入れる。ret は、取り出した番地がまとめた戻り先なら、そこへ移る。
//! 表を引く間接の jmp（switch）は、表の飛び先をまとめに入れ、実際の飛び先と比べて移る
use super::*;

/// まとめるブロックの数と命令の数の上限
const MAX_BLOCKS: usize = 32;
const MAX_INSNS: usize = 1024;

/// 1 つの基本ブロックの命令と終わり方
struct Block {
    insns: Vec<Instruction>,
    term: Term,
    /// 終わりが表を引く間接の jmp なら、表から読んだ飛び先（候補。実際の飛び先とは実行のときに比べる）
    table: Vec<u64>,
}

/// jmp [index*w + 表] の表から、飛び先の候補を読む（近くの番地で、命令として読めるものだけ。最大 64 個）
fn table_targets(mem: &Memory, i: &Instruction) -> Vec<u64> {
    let w = stack::width(i) as u64;
    if i.flow_control() != FlowControl::IndirectBranch || i.op_kind(0) != OpKind::Memory || i.memory_base() != Register::None || i.memory_index() == Register::None || i.memory_index_scale() as u64 != w {
        return Vec::new();
    }
    let mut v = Vec::new();
    for k in 0..64 {
        let Ok(t) = mem.read_n(i.memory_displacement64().wrapping_add(k * w), w as usize) else { break };
        if t.abs_diff(i.ip()) > 0x10_0000 || mem.read_n(t, 1).is_err() {
            break;
        }
        if !v.contains(&t) {
            v.push(t);
        }
    }
    v
}

/// start から 1 つの基本ブロックを読む。先頭の命令から変換できないときは None
fn scan(mem: &Memory, bits: u32, start: u64, max_block: usize) -> Option<Block> {
    let mut insns: Vec<Instruction> = Vec::new();
    let mut rip = start;
    let term;
    loop {
        let bytes = mem.fetch(rip, 16).ok()?;
        let mut dec = Decoder::with_ip(bits, bytes, rip, DecoderOptions::NONE);
        let insn = dec.decode();
        if insn.is_invalid() {
            if insns.is_empty() {
                return None;
            }
            term = Term::ExitAt(rip);
            break;
        }
        let next = insn.next_ip();
        if insn.flow_control() != FlowControl::Next {
            if insn.mnemonic() == M::Jmp && matches!(insn.op_kind(0), OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64) {
                term = Term::Jmp(insn.near_branch_target());
            } else if is_jcc(&insn) {
                term = Term::Jcc(insn.condition_code(), insn.near_branch_target(), next);
            } else if stack::term_ok(&insn) {
                term = Term::Flow(insn);
            } else {
                if insns.is_empty() {
                    return None;
                }
                term = Term::ExitAt(rip);
            }
            break;
        }
        if insn_rw(&insn).is_none() {
            if insns.is_empty() {
                return None;
            }
            term = Term::ExitAt(rip);
            break;
        }
        insns.push(insn);
        rip = next;
        if insns.len() >= max_block {
            term = Term::ExitAt(rip);
            break;
        }
    }
    let table = match &term {
        Term::Flow(i) => table_targets(mem, i),
        _ => Vec::new(),
    };
    Some(Block { insns, term, table })
}

/// ブロックの命令（終わりの分岐は除く）を asm.body に書く。作れなければ None
fn emit_body(asm: &mut Asm, b: &Block) -> Option<()> {
    // フラグの生死（後ろ向き。ブロック末では全フラグ生存とみなす）。読まれる前に必ず上書きされるフラグは作らない
    let n = b.insns.len();
    let mut live = vec![0u8; n];
    let mut need = FALL;
    for k in (0..n).rev() {
        let f = insn_rw(&b.insns[k])?;
        live[k] = f.emit & need;
        need = (need & !f.kill) | f.read;
    }
    for (k, insn) in b.insns.iter().enumerate() {
        asm.flag_mask = live[k];
        emit_insn(asm, insn, insn.ip(), k as u32)?;
    }
    asm.flag_mask = FALL;
    Some(())
}

/// target へ移る。まとめた中なら、命令の数を足して x87 の先頭を進め、上限までは PC を置いて頭の loop へ。外なら rip を置いて戻る。
/// depth は今の位置から頭の loop までの深さ
fn goto(asm: &mut Asm, idx: &[u64], target: u64, count: u32, depth: u32) {
    let Some(j) = idx.iter().position(|&a| a == target) else {
        asm.emit_exit(target, count);
        return;
    };
    let fdelta = asm.fdelta;
    asm.body.extend([W::LocalGet(CNT), W::I32Const(count as i32), W::I32Add, W::LocalSet(CNT)]);
    if asm.fpu {
        asm.fpu_sync();
        asm.body.extend([W::LocalGet(FT), W::I32Const(fdelta), W::I32Add, W::I32Const(7), W::I32And, W::LocalSet(FT)]);
    }
    asm.body.extend([W::I32Const(j as i32), W::LocalSet(PC)]);
    asm.body.extend([W::LocalGet(CNT), W::I32Const(LOOP_LIMIT), W::I32LtU, W::BrIf(depth)]);
    // 上限: 見張りに戻すため、移り先を rip にして抜ける（命令の数と x87 は書き戻し済み）
    asm.fdelta = 0;
    asm.emit_exit(target, 0);
    asm.fdelta = fdelta;
}

/// 飛び先が決まっている call か
fn direct_call(i: &Instruction) -> bool {
    i.flow_control() == FlowControl::Call && matches!(i.op_kind(0), OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64)
}

/// 終わりの call、ret。呼ぶ先や戻り先がまとめた中なら、Rust に戻らずに移る。ほかは stack::emit_term と同じ
fn emit_flow(asm: &mut Asm, idx: &[u64], rets: &[u64], table: &[u64], i: &Instruction, count: u32, depth: u32) {
    let (w, at, before) = (stack::width(i), i.ip(), count - 1);
    if direct_call(i) && idx.contains(&i.near_branch_target()) {
        asm.body.extend([W::I64Const(i.next_ip() as i64), W::LocalSet(B)]);
        asm.push_b(w, at, before);
        goto(asm, idx, i.near_branch_target(), count, depth);
    } else if i.flow_control() == FlowControl::Return && !rets.is_empty() {
        let extra = if i.op_count() == 1 { i.immediate(0) } else { 0 };
        asm.pop_b(w, extra, at, before);
        for &r in rets {
            asm.body.extend([W::LocalGet(B), W::I64Const(r as i64), W::I64Eq, W::If(BlockType::Empty)]);
            goto(asm, idx, r, count, depth + 1);
            asm.body.push(W::End);
        }
        asm.emit_exit_dyn(B, count);
    } else if table.iter().any(|t| idx.contains(t)) {
        // 飛び先を R に読み（読めなければ中断）、まとめた中の候補と比べる
        asm.push_operand(&operand(i, 0).expect("term_ok で確かめ済み"), at, before);
        asm.body.push(W::LocalSet(R));
        for &t in table.iter().filter(|t| idx.contains(t)) {
            asm.body.extend([W::LocalGet(R), W::I64Const(t as i64), W::I64Eq, W::If(BlockType::Empty)]);
            goto(asm, idx, t, count, depth + 1);
            asm.body.push(W::End);
        }
        asm.emit_exit_dyn(R, count);
    } else {
        stack::emit_term(asm, i, count);
    }
}

/// start から、分岐でつながるブロックをまとめて変換する。先頭のブロックが作れないときは None
pub fn compile_block(mem: &Memory, bits: u32, start: u64, max_block: usize) -> Option<Compiled> {
    // 分岐の先を幅優先でたどって集める（call、ret、間接の分岐の先はたどらない）
    let mut addrs = vec![start];
    let mut blocks: Vec<Option<Block>> = Vec::new();
    let mut total = 0;
    let mut k = 0;
    while k < addrs.len() {
        let b = scan(mem, bits, addrs[k], max_block);
        if k == 0 && b.is_none() {
            return None;
        }
        if let Some(b) = &b {
            total += b.insns.len();
            let next = match b.term {
                Term::Jmp(t) => vec![t],
                Term::Jcc(_, t, n) => vec![t, n],
                Term::Flow(i) if direct_call(&i) => vec![i.near_branch_target(), i.next_ip()],
                _ => b.table.clone(),
            };
            for t in next {
                if addrs.len() < MAX_BLOCKS && total < MAX_INSNS && !addrs.contains(&t) {
                    addrs.push(t);
                }
            }
        }
        blocks.push(b);
        k += 1;
    }
    let fpu = blocks.iter().flatten().flat_map(|b| &b.insns).any(|i| x87::is_x87(i.mnemonic()));
    let mut asm = Asm { body: Vec::new(), off: offs(), pages_ptr: mem.pages_ptr(), code_ptr: mem.code_pages_ptr(), bits, flag_mask: FALL, fpu, fdelta: 0 };
    // 各ブロックの命令を別々に作る。作れないブロック（先頭以外）は外として扱う
    let mut bodies = Vec::new();
    let mut idx = Vec::new();
    for (k, b) in blocks.into_iter().enumerate() {
        let Some(b) = b else { continue };
        asm.fdelta = 0;
        let ok = emit_body(&mut asm, &b).is_some();
        let body = std::mem::take(&mut asm.body);
        if !ok {
            if k == 0 {
                // insn_rw が対応と判断したのに生成できない → 安全のため作らない
                return None;
            }
            continue;
        }
        idx.push(addrs[k]);
        bodies.push((body, asm.fdelta, b));
    }
    // まとめた中の、直接の call の戻り先（呼ぶ先もまとめた中のもの）
    let rets: Vec<u64> = bodies.iter().filter_map(|(_, _, b)| match b.term {
        Term::Flow(i) if direct_call(&i) && idx.contains(&i.near_branch_target()) && idx.contains(&i.next_ip()) => Some(i.next_ip()),
        _ => None,
    }).collect();
    // 組み立て: loop { block … block { br_table PC } ブロック 0 … } の入れ子
    let n = bodies.len() as u32;
    if fpu {
        asm.cpu();
        asm.body.push(W::I32Load(MemArg { offset: offset_of!(Cpu, fpu_top) as u64, align: 2, memory_index: 0 }));
        asm.body.push(W::LocalSet(FT));
    }
    asm.body.push(W::Loop(BlockType::Empty));
    for _ in 0..n {
        asm.body.push(W::Block(BlockType::Empty));
    }
    asm.body.push(W::LocalGet(PC));
    let targets: Vec<u32> = (0..n).collect();
    asm.body.push(W::BrTable(targets.into(), n - 1));
    for (k, (body, fdelta, b)) in bodies.into_iter().enumerate() {
        asm.body.push(W::End);
        asm.body.extend(body);
        asm.fdelta = fdelta;
        let count = b.insns.len() as u32;
        // ブロック k の後ろでは、残りの block（n-1-k 個）の内側にいる
        let depth = n - 1 - k as u32;
        match b.term {
            Term::ExitAt(a) | Term::Jmp(a) => goto(&mut asm, &idx, a, count, depth),
            // 終わりの命令も 1 つと数える
            Term::Flow(i) => emit_flow(&mut asm, &idx, &rets, &b.table, &i, count + 1, depth),
            Term::Jcc(cc, t, next) => {
                asm.push_cond(cc);
                asm.body.push(W::If(BlockType::Empty));
                goto(&mut asm, &idx, t, count, depth + 1);
                asm.body.push(W::End);
                goto(&mut asm, &idx, next, count, depth);
            }
        }
    }
    asm.body.push(W::End);
    asm.body.push(W::Unreachable);
    Some(finish(asm))
}
