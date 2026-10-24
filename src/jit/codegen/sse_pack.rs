//! SSE の packed の並べ替え（unpckhps、punpckldq、pshufd、movsldup、movshdup）、packed の比較（pcmpeqd、pcmpgtd）、rsqrtps、rcpps、movntps、prefetch、半分の転送（movlps、movhps）、packed の整数との変換（cvtdq2ps、cvtps2dq、cvttps2dq）、addsubps を wasm に変換する。
//! インタープリター（src/sse2.rs）と同じ結果にする。変換の範囲外と NaN は x86 と同じ 0x80000000
use super::*;

fn xmm(r: Register) -> Option<usize> {
    crate::cpu::xmm_index(r).ok()
}
fn is_xmm(i: &Instruction, n: u32) -> bool {
    i.op_kind(n) == OpKind::Register && xmm(i.op_register(n)).is_some()
}
fn is_mem(i: &Instruction, n: u32, size: usize) -> bool {
    i.op_kind(n) == OpKind::Memory && operand(i, n).is_some() && i.memory_size().size() == size
}
fn xmm_arg(idx: usize) -> MemArg {
    MemArg { offset: off_xmm(idx), align: 4, memory_index: 0 }
}
/// 4 バイトの組を選ぶ i8x16.shuffle の並び（0〜3 は a、4〜7 は b）
fn lanes32(sel: [u8; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (k, s) in sel.iter().enumerate() {
        for b in 0..4 {
            out[k * 4 + b] = s * 4 + b as u8;
        }
    }
    out
}

/// 変換できるか（どれもフラグは変えない）
pub(super) fn rw(i: &Instruction) -> Option<Rw> {
    use M::*;
    let ok = match i.mnemonic() {
        Prefetcht0 | Prefetcht1 | Prefetcht2 | Prefetchnta => true,
        Movntps | Movntdq | Movntpd => is_mem(i, 0, 16) && is_xmm(i, 1),
        Punpckldq | Punpckhdq | Pcmpeqd | Pcmpgtd | Rsqrtps | Rcpps | Unpckhps | Addsubps | Cvtdq2ps | Cvtps2dq | Cvttps2dq | Movsldup | Movshdup | Pshufd => is_xmm(i, 0) && (is_xmm(i, 1) || is_mem(i, 1, 16)),
        // xmm の半分とメモリの 8 バイト
        Cvtdq2pd => is_xmm(i, 0) && (is_xmm(i, 1) || is_mem(i, 1, 8)),
        Movlps | Movhps | Movlpd | Movhpd => (is_xmm(i, 0) && is_mem(i, 1, 8)) || (is_mem(i, 0, 8) && is_xmm(i, 1)),
        _ => false,
    };
    ok.then_some(Rw { kill: 0, emit: 0, read: 0 })
}

pub(super) fn emit(asm: &mut Asm, i: &Instruction, insn_addr: u64, count: u32) -> Option<()> {
    let m = i.mnemonic();
    match m {
        M::Prefetcht0 | M::Prefetcht1 | M::Prefetcht2 | M::Prefetchnta => {}
        // キャッシュを通さない書き込みは、ふつうの 16 バイトの書き込みと同じ
        M::Movntps | M::Movntdq | M::Movntpd => {
            let s = xmm(i.op_register(1))?;
            let Opnd::Mem(mo, _) = operand(i, 0)? else { return None };
            asm.calc_addr(&mo);
            asm.resolve_ptr_w(16, insn_addr, count);
            asm.body.push(W::LocalGet(HP));
            asm.cpu();
            asm.body.extend([W::V128Load(xmm_arg(s)), W::V128Store(MemArg { offset: 0, align: 0, memory_index: 0 })]);
        }
        // 1 / sqrt(x)、1 / x（インタープリターと同じく、近似ではなく正しく割る）
        M::Rsqrtps | M::Rcpps => {
            let d = xmm(i.op_register(0))?;
            asm.cpu();
            let al = xmm_ptr(asm, i, 1, 16, insn_addr, count)?;
            asm.body.push(v128ld(al));
            if m == M::Rsqrtps {
                asm.body.push(W::F32x4Sqrt);
            }
            asm.body.extend([W::LocalSet(V0), W::V128Const(i128::from_le_bytes(u32x4(1.0f32.to_bits()))), W::LocalGet(V0), W::F32x4Div]);
            asm.body.push(W::V128Store(xmm_arg(d)));
        }
        // 下の 2 つの i32 → f64 2 つ
        M::Cvtdq2pd => {
            let d = xmm(i.op_register(0))?;
            asm.cpu();
            if is_xmm(i, 1) {
                asm.cpu();
                asm.body.push(W::V128Load(xmm_arg(xmm(i.op_register(1))?)));
            } else {
                xmm_ptr(asm, i, 1, 8, insn_addr, count)?;
                asm.body.push(W::V128Load64Zero(MemArg { offset: 0, align: 0, memory_index: 0 }));
            }
            asm.body.extend([W::F64x2ConvertLowI32x4S, W::V128Store(xmm_arg(d))]);
        }
        // メモリの 8 バイトを xmm の下（movlps）か上（movhps）へ。残りの半分はそのまま
        M::Movlps | M::Movhps | M::Movlpd | M::Movhpd if is_xmm(i, 0) => {
            let d = xmm(i.op_register(0))?;
            let lane = if matches!(m, M::Movhps | M::Movhpd) { 1 } else { 0 };
            asm.cpu();
            let Opnd::Mem(mo, _) = operand(i, 1)? else { return None };
            asm.calc_addr(&mo);
            asm.resolve_ptr(8, insn_addr, count);
            asm.body.push(W::LocalGet(HP));
            asm.cpu();
            asm.body.push(W::V128Load(xmm_arg(d)));
            asm.body.push(W::V128Load64Lane { memarg: MemArg { offset: 0, align: 0, memory_index: 0 }, lane });
            asm.body.push(W::V128Store(xmm_arg(d)));
        }
        // xmm の下か上の 8 バイトをメモリへ
        M::Movlps | M::Movhps | M::Movlpd | M::Movhpd => {
            let s = xmm(i.op_register(1))?;
            let lane = if matches!(m, M::Movhps | M::Movhpd) { 1 } else { 0 };
            let Opnd::Mem(mo, _) = operand(i, 0)? else { return None };
            asm.calc_addr(&mo);
            asm.resolve_ptr_w(8, insn_addr, count);
            asm.body.push(W::LocalGet(HP));
            asm.cpu();
            asm.body.push(W::V128Load(xmm_arg(s)));
            asm.body.push(W::V128Store64Lane { memarg: MemArg { offset: 0, align: 0, memory_index: 0 }, lane });
        }
        // 引数 1 つの形（もう一方だけを読む）: 並べ替えと変換
        M::Movsldup | M::Movshdup | M::Pshufd | M::Cvtdq2ps | M::Cvtps2dq | M::Cvttps2dq => {
            let d = xmm(i.op_register(0))?;
            asm.cpu();
            let al = xmm_ptr(asm, i, 1, 16, insn_addr, count)?;
            asm.body.push(v128ld(al));
            match m {
                M::Movsldup | M::Movshdup | M::Pshufd => {
                    let sel = match m {
                        M::Movsldup => [0, 0, 2, 2],
                        M::Movshdup => [1, 1, 3, 3],
                        _ => {
                            let k = i.immediate8();
                            [k & 3, (k >> 2) & 3, (k >> 4) & 3, (k >> 6) & 3]
                        }
                    };
                    asm.body.extend([W::LocalTee(V0), W::LocalGet(V0), W::I8x16Shuffle(lanes32(sel))]);
                }
                M::Cvtdq2ps => asm.body.push(W::F32x4ConvertI32x4S),
                _ => {
                    // 丸め（cvtps2dq は最も近い偶数、cvttps2dq は 0 の方向）。範囲の外と NaN は 0x80000000
                    if m == M::Cvtps2dq {
                        asm.body.push(W::F32x4Nearest);
                    }
                    asm.body.extend([W::LocalTee(V0), W::I32x4TruncSatF32x4S]);
                    asm.body.extend([W::V128Const(i128::from_le_bytes(u32x4(0x8000_0000)))]);
                    asm.body.extend([W::LocalGet(V0), W::V128Const(i128::from_le_bytes(u32x4((-2147483648.0f32).to_bits()))), W::F32x4Ge]);
                    asm.body.extend([W::LocalGet(V0), W::V128Const(i128::from_le_bytes(u32x4(2147483648.0f32.to_bits()))), W::F32x4Lt, W::V128And]);
                    asm.body.push(W::V128Bitselect);
                }
            }
            asm.body.push(W::V128Store(xmm_arg(d)));
        }
        // 2 つの引数（a = 宛先、b = もう一方）
        M::Unpckhps | M::Addsubps | M::Punpckldq | M::Punpckhdq | M::Pcmpeqd | M::Pcmpgtd => {
            let d = xmm(i.op_register(0))?;
            asm.cpu();
            asm.cpu();
            asm.body.push(W::V128Load(xmm_arg(d)));
            let al = xmm_ptr(asm, i, 1, 16, insn_addr, count)?;
            asm.body.push(v128ld(al));
            if matches!(m, M::Unpckhps | M::Punpckhdq) {
                asm.body.push(W::I8x16Shuffle(lanes32([2, 6, 3, 7])));
            } else if m == M::Punpckldq {
                asm.body.push(W::I8x16Shuffle(lanes32([0, 4, 1, 5])));
            } else if m == M::Pcmpeqd {
                asm.body.push(W::I32x4Eq);
            } else if m == M::Pcmpgtd {
                asm.body.push(W::I32x4GtS);
            } else {
                // 偶数の組は a - b、奇数の組は a + b
                asm.body.extend([W::LocalSet(V1), W::LocalTee(V0), W::LocalGet(V1), W::F32x4Sub, W::LocalGet(V0), W::LocalGet(V1), W::F32x4Add]);
                asm.body.push(W::I8x16Shuffle(lanes32([0, 5, 2, 7])));
            }
            asm.body.push(W::V128Store(xmm_arg(d)));
        }
        _ => return None,
    }
    Some(())
}

/// 4 つの組がどれも v の 16 バイト
fn u32x4(v: u32) -> [u8; 16] {
    let mut out = [0u8; 16];
    for k in 0..4 {
        out[k * 4..k * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}
