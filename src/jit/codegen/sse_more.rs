//! SSE の型の変換（cvt）、整数のレジスタとの行き来（movd、movq）、並べ替え（shufps、unpcklps、shufpd、unpcklpd など）、packed の比較とシフト、movmsk を wasm に変換する。
//! インタープリター（src/sse.rs、src/sse2.rs）と同じ結果にする。cvtt の範囲外と NaN は x86 と同じ「最小の負の数」
use super::*;

fn xmm(r: Register) -> Option<usize> {
    crate::cpu::xmm_index(r).ok()
}
fn is_xmm(i: &Instruction, n: u32) -> bool {
    i.op_kind(n) == OpKind::Register && xmm(i.op_register(n)).is_some()
}
/// 32 か 64 ビットの汎用レジスタか
fn is_gpr(i: &Instruction, n: u32) -> bool {
    i.op_kind(n) == OpKind::Register && matches!(gpr(i.op_register(n)), Some((_, 4 | 8)))
}
fn is_mem(i: &Instruction, n: u32, sizes: &[usize]) -> bool {
    i.op_kind(n) == OpKind::Memory && operand(i, n).is_some() && sizes.contains(&i.memory_size().size())
}

/// 変換できるか（どれもフラグは変えない）
pub(super) fn rw(i: &Instruction) -> Option<Rw> {
    use M::*;
    let ok = match i.mnemonic() {
        Cvtsi2ss | Cvtsi2sd => is_xmm(i, 0) && (is_gpr(i, 1) || is_mem(i, 1, &[4, 8])),
        Cvtss2sd => is_xmm(i, 0) && (is_xmm(i, 1) || is_mem(i, 1, &[4])),
        Cvtsd2ss => is_xmm(i, 0) && (is_xmm(i, 1) || is_mem(i, 1, &[8])),
        Cvttss2si => is_gpr(i, 0) && (is_xmm(i, 1) || is_mem(i, 1, &[4])),
        Cvttsd2si => is_gpr(i, 0) && (is_xmm(i, 1) || is_mem(i, 1, &[8])),
        Movd | Movq => {
            let s = if i.mnemonic() == Movd { 4 } else { 8 };
            let side = |n| (is_gpr(i, n) && i.op_register(n).size() == s) || is_mem(i, n, &[s]);
            (is_xmm(i, 0) && (side(1) || (s == 8 && is_xmm(i, 1)))) || (is_xmm(i, 1) && side(0))
        }
        Unpcklps | Shufps => is_xmm(i, 0) && (is_xmm(i, 1) || is_mem(i, 1, &[16])),
        Movhlps | Movlhps => is_xmm(i, 0) && is_xmm(i, 1),
        Cvtps2pd => is_xmm(i, 0) && (is_xmm(i, 1) || is_mem(i, 1, &[8])),
        Cvtpd2ps | Unpcklpd | Unpckhpd | Punpcklqdq | Punpckhqdq | Shufpd | Cmppd | Cmpps => is_xmm(i, 0) && (is_xmm(i, 1) || is_mem(i, 1, &[16])),
        // シフトは即値の数だけ
        Psllq | Psrlq | Pslld | Psrld | Psrad => is_xmm(i, 0) && i.op_kind(1) == OpKind::Immediate8,
        Movmskps | Movmskpd => is_gpr(i, 0) && is_xmm(i, 1),
        Pextrw => is_gpr(i, 0) && is_xmm(i, 1),
        Cvtpd2dq | Cvttpd2dq => is_xmm(i, 0) && (is_xmm(i, 1) || is_mem(i, 1, &[16])),
        _ => return sse_pack::rw(i),
    };
    ok.then_some(Rw { kill: 0, emit: 0, read: 0 })
}

fn xmm_arg(idx: usize, align: u32) -> MemArg {
    MemArg { offset: off_xmm(idx), align, memory_index: 0 }
}

/// i8x16.shuffle の並び（4 バイトの組の番号 0〜3 は a、4〜7 は b）
fn lanes32(sel: [u8; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (k, s) in sel.iter().enumerate() {
        for b in 0..4 {
            out[k * 4 + b] = s * 4 + b as u8;
        }
    }
    out
}

pub(super) fn emit(asm: &mut Asm, i: &Instruction, insn_addr: u64, count: u32) -> Option<()> {
    let m = i.mnemonic();
    match m {
        // 整数 → 浮動小数点（下位だけを書き、上位は残す）
        M::Cvtsi2ss | M::Cvtsi2sd => {
            let d = xmm(i.op_register(0))?;
            let src = operand(i, 1)?;
            let wide = match src {
                Opnd::Reg(_, s) | Opnd::Mem(_, s) => s == 8,
                Opnd::Imm(_) => return None,
            };
            asm.push_operand(&src, insn_addr, count);
            asm.body.push(W::LocalSet(A));
            asm.cpu();
            asm.body.push(W::LocalGet(A));
            if !wide {
                asm.body.push(W::I32WrapI64);
            }
            match (m == M::Cvtsi2ss, wide) {
                (true, true) => asm.body.extend([W::F32ConvertI64S, W::F32Store(xmm_arg(d, 2))]),
                (true, false) => asm.body.extend([W::F32ConvertI32S, W::F32Store(xmm_arg(d, 2))]),
                (false, true) => asm.body.extend([W::F64ConvertI64S, W::F64Store(xmm_arg(d, 3))]),
                (false, false) => asm.body.extend([W::F64ConvertI32S, W::F64Store(xmm_arg(d, 3))]),
            }
        }
        M::Cvtss2sd | M::Cvtsd2ss => {
            let d = xmm(i.op_register(0))?;
            let ss = m == M::Cvtss2sd;
            let al = xmm_ptr(asm, i, 1, if ss { 4 } else { 8 }, insn_addr, count)?;
            if ss {
                asm.body.extend([f32ld(al), W::F64PromoteF32, W::LocalSet(D0)]);
                asm.cpu();
                asm.body.extend([W::LocalGet(D0), W::F64Store(xmm_arg(d, 3))]);
            } else {
                asm.body.extend([f64ld(al), W::F32DemoteF64, W::LocalSet(F0)]);
                asm.cpu();
                asm.body.extend([W::LocalGet(F0), W::F32Store(xmm_arg(d, 2))]);
            }
        }
        // 浮動小数点 → 整数（0 方向へ切り捨て）。範囲外と NaN は最小の負の数
        M::Cvttss2si | M::Cvttsd2si => {
            let (idx, size) = gpr(i.op_register(0))?;
            let ss = m == M::Cvttss2si;
            let al = xmm_ptr(asm, i, 1, if ss { 4 } else { 8 }, insn_addr, count)?;
            if ss {
                asm.body.extend([f32ld(al), W::F64PromoteF32]);
            } else {
                asm.body.push(f64ld(al));
            }
            asm.body.extend([W::F64Trunc, W::LocalSet(D0)]);
            let lim = if size == 8 { 9.223372036854775808e18 } else { 2147483648.0 };
            asm.body.push(W::LocalGet(D0));
            if size == 8 {
                asm.body.extend([W::I64TruncSatF64S, W::I64Const(i64::MIN)]);
            } else {
                asm.body.extend([W::I32TruncSatF64S, W::I64ExtendI32U, W::I64Const(0x8000_0000)]);
            }
            // 範囲の中か（NaN は比べるとどれも偽になるので外）
            let (lo, hi): (f64, f64) = (-lim, lim);
            asm.body.extend([W::LocalGet(D0), W::F64Const(lo.into()), W::F64Ge, W::LocalGet(D0), W::F64Const(hi.into()), W::F64Lt, W::I32And, W::Select]);
            asm.store_reg(idx, size);
        }
        M::Movd | M::Movq => {
            let s = if m == M::Movd { 4 } else { 8 };
            if is_xmm(i, 0) {
                // xmm ← 汎用レジスタ、メモリ、xmm（movq）。上位は 0
                let d = xmm(i.op_register(0))?;
                if is_xmm(i, 1) {
                    asm.cpu();
                    asm.body.extend([W::I64Load(xmm_arg(xmm(i.op_register(1))?, 3)), W::LocalSet(A)]);
                } else {
                    asm.push_operand(&operand(i, 1)?, insn_addr, count);
                    asm.body.push(W::LocalSet(A));
                }
                asm.cpu();
                asm.body.extend([W::V128Const(0), W::V128Store(xmm_arg(d, 4))]);
                asm.cpu();
                asm.body.extend([W::LocalGet(A), W::I64Store(xmm_arg(d, 3))]);
            } else {
                // 汎用レジスタ、メモリ ← xmm の下位
                let sx = xmm(i.op_register(1))?;
                match operand(i, 0)? {
                    Opnd::Reg(idx, _) => {
                        asm.cpu();
                        asm.body.push(W::I64Load(xmm_arg(sx, 3)));
                        mask_i64(&mut asm.body, s);
                        asm.store_reg(idx, s);
                    }
                    Opnd::Mem(mo, _) => {
                        asm.calc_addr(&mo);
                        asm.resolve_ptr_w(s, insn_addr, count);
                        asm.body.push(W::LocalGet(HP));
                        asm.cpu();
                        asm.body.extend([W::I64Load(xmm_arg(sx, 3)), store_op(s)]);
                    }
                    Opnd::Imm(_) => return None,
                }
            }
        }
        // 並べ替え: a = 宛先、b = もう一方
        M::Unpcklps | M::Shufps | M::Movhlps | M::Movlhps => {
            let d = xmm(i.op_register(0))?;
            asm.cpu(); // 書く先
            asm.cpu();
            asm.body.push(W::V128Load(xmm_arg(d, 4)));
            let al = xmm_ptr(asm, i, 1, 16, insn_addr, count)?;
            asm.body.push(v128ld(al));
            let sel = match m {
                M::Unpcklps => [0, 4, 1, 5],
                M::Movhlps => [6, 7, 2, 3],
                M::Movlhps => [0, 1, 4, 5],
                _ => {
                    let k = i.immediate8();
                    [k & 3, (k >> 2) & 3, 4 + ((k >> 4) & 3), 4 + ((k >> 6) & 3)]
                }
            };
            asm.body.push(W::I8x16Shuffle(lanes32(sel)));
            asm.body.push(W::V128Store(xmm_arg(d, 4)));
        }
        // f32 の下位 2 つ → f64 2 つ
        M::Cvtps2pd => {
            let d = xmm(i.op_register(0))?;
            asm.cpu();
            if is_xmm(i, 1) {
                asm.cpu();
                asm.body.push(W::V128Load(xmm_arg(xmm(i.op_register(1))?, 4)));
            } else {
                xmm_ptr(asm, i, 1, 8, insn_addr, count)?;
                asm.body.push(W::V128Load64Zero(MemArg { offset: 0, align: 0, memory_index: 0 }));
            }
            asm.body.extend([W::F64x2PromoteLowF32x4, W::V128Store(xmm_arg(d, 4))]);
        }
        // f64 2 つ → f32 の下位 2 つ（上位は 0）
        M::Cvtpd2ps => {
            let d = xmm(i.op_register(0))?;
            asm.cpu();
            let al = xmm_ptr(asm, i, 1, 16, insn_addr, count)?;
            asm.body.extend([v128ld(al), W::F32x4DemoteF64x2Zero, W::V128Store(xmm_arg(d, 4))]);
        }
        // 64 ビットの組の並べ替えと、packed の比較（a = 宛先、b = もう一方）
        M::Unpcklpd | M::Unpckhpd | M::Punpcklqdq | M::Punpckhqdq | M::Shufpd | M::Cmppd | M::Cmpps => {
            let d = xmm(i.op_register(0))?;
            asm.cpu();
            asm.cpu();
            asm.body.push(W::V128Load(xmm_arg(d, 4)));
            let al = xmm_ptr(asm, i, 1, 16, insn_addr, count)?;
            asm.body.push(v128ld(al));
            let sel = match m {
                M::Unpcklpd | M::Punpcklqdq => Some([0, 2]),
                M::Unpckhpd | M::Punpckhqdq => Some([1, 3]),
                M::Shufpd => Some([i.immediate8() & 1, 2 + ((i.immediate8() >> 1) & 1)]),
                _ => None,
            };
            if let Some(sel) = sel {
                asm.body.push(W::I8x16Shuffle(lanes32([sel[0] * 2, sel[0] * 2 + 1, sel[1] * 2, sel[1] * 2 + 1])));
            } else {
                cmp_packed(asm, m == M::Cmppd, i.immediate8());
            }
            asm.body.push(W::V128Store(xmm_arg(d, 4)));
        }
        // 即値のシフト。幅以上は 0（算術の右は符号で埋める）。wasm は数を幅で割った余りにするので、ここで分ける
        M::Psllq | M::Psrlq | M::Pslld | M::Psrld | M::Psrad => {
            let d = xmm(i.op_register(0))?;
            let n = i.immediate8() as u32;
            let bits = if matches!(m, M::Psllq | M::Psrlq) { 64 } else { 32 };
            asm.cpu();
            if n >= bits && m != M::Psrad {
                asm.body.push(W::V128Const(0));
            } else {
                asm.cpu();
                asm.body.extend([W::V128Load(xmm_arg(d, 4)), W::I32Const(n.min(bits - 1) as i32)]);
                asm.body.push(match m {
                    M::Psllq => W::I64x2Shl,
                    M::Psrlq => W::I64x2ShrU,
                    M::Pslld => W::I32x4Shl,
                    M::Psrld => W::I32x4ShrU,
                    _ => W::I32x4ShrS,
                });
            }
            asm.body.push(W::V128Store(xmm_arg(d, 4)));
        }
        // 各組の符号のビットを並べる
        M::Movmskps | M::Movmskpd => {
            let (idx, size) = gpr(i.op_register(0))?;
            asm.cpu();
            asm.body.push(W::V128Load(xmm_arg(xmm(i.op_register(1))?, 4)));
            asm.body.extend([if m == M::Movmskps { W::I32x4Bitmask } else { W::I64x2Bitmask }, W::I64ExtendI32U]);
            asm.store_reg(idx, size);
        }
        // 16 ビットの組を 1 つ取り出す（上位は 0）
        M::Pextrw => {
            let (idx, size) = gpr(i.op_register(0))?;
            asm.cpu();
            asm.body.push(W::V128Load(xmm_arg(xmm(i.op_register(1))?, 4)));
            asm.body.extend([W::I16x8ExtractLaneU(i.immediate8() & 7), W::I64ExtendI32U]);
            asm.store_reg(idx, size);
        }
        // f64 2 つ → i32 の下位 2 つ（上位は 0）。cvtpd2dq は最も近い偶数、cvttpd2dq は 0 の方向に丸める。範囲の外と NaN は 0x80000000
        M::Cvtpd2dq | M::Cvttpd2dq => {
            let d = xmm(i.op_register(0))?;
            asm.cpu();
            let al = xmm_ptr(asm, i, 1, 16, insn_addr, count)?;
            asm.body.extend([v128ld(al), W::LocalSet(V0), W::V128Const(0)]);
            for k in 0..2 {
                asm.body.extend([W::LocalGet(V0), W::F64x2ExtractLane(k)]);
                if m == M::Cvtpd2dq {
                    asm.body.push(W::F64Nearest);
                }
                asm.body.push(W::LocalSet(D0));
                asm.body.extend([W::LocalGet(D0), W::I32TruncSatF64S, W::I32Const(0x8000_0000u32 as i32)]);
                asm.body.extend([W::LocalGet(D0), W::F64Const((-2147483648.0f64).into()), W::F64Ge, W::LocalGet(D0), W::F64Const(2147483648.0f64.into()), W::F64Lt, W::I32And, W::Select]);
                asm.body.push(W::I32x4ReplaceLane(k));
            }
            asm.body.push(W::V128Store(xmm_arg(d, 4)));
        }
        _ => return sse_pack::emit(asm, i, insn_addr, count),
    }
    Some(())
}

/// スタックの a、b を、cmpps、cmppd の述語 k（インタープリターの cmp_f と同じ 8 つ）で比べ、真の組をすべて 1 にする
fn cmp_packed(asm: &mut Asm, pd: bool, k: u8) {
    let (eq, lt, le, ne) = if pd { (W::F64x2Eq, W::F64x2Lt, W::F64x2Le, W::F64x2Ne) } else { (W::F32x4Eq, W::F32x4Lt, W::F32x4Le, W::F32x4Ne) };
    let un = |asm: &mut Asm, ne: W<'static>| {
        // NaN は自分と等しくない
        asm.body.extend([W::LocalSet(V1), W::LocalTee(V0), W::LocalGet(V0), ne.clone(), W::LocalGet(V1), W::LocalGet(V1), ne, W::V128Or]);
    };
    match k & 7 {
        0 => asm.body.push(eq),
        1 => asm.body.push(lt),
        2 => asm.body.push(le),
        3 => un(asm, ne),
        4 => asm.body.push(ne),
        5 => asm.body.extend([lt, W::V128Not]),
        6 => asm.body.extend([le, W::V128Not]),
        _ => {
            un(asm, ne);
            asm.body.push(W::V128Not);
        }
    }
}
