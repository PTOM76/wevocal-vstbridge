//! x87 の命令を wasm に変換する。インタープリター（src/x87.rs）と同じく値は f64 で持ち、結果をビット単位で合わせる。
//! スタックの先頭はブロックの頭で FT に読み、ブロックの中の出し入れは Asm::fdelta で数える（抜ける所で書き戻す）
use super::*;

/// 変換する x87 の命令か
pub(super) fn is_x87(m: M) -> bool {
    use M::*;
    matches!(
        m,
        Fld | Fild | Fld1 | Fldz | Fldpi | Fldl2e | Fldl2t | Fldlg2 | Fldln2 | Fst | Fstp | Fist | Fistp | Fisttp | Fadd | Fsub | Fsubr | Fmul | Fdiv | Fdivr | Faddp | Fsubp | Fsubrp | Fmulp | Fdivp | Fdivrp | Fiadd | Fisub | Fisubr | Fimul | Fidiv | Fidivr | Fchs | Fabs | Fsqrt | Frndint | Fxch | Fcom | Fcomp | Fcompp | Fucom | Fucomp | Fucompp | Ficom | Ficomp | Fcomi | Fcomip | Fucomi | Fucomip | Fnstsw | Fnstcw | Fldcw | Fnclex | Wait | Fsin | Fcos | Fsincos | Fptan | Fpatan | Fyl2x | Fyl2xp1 | F2xm1 | Fscale | Fprem | Fprem1
    )
}

/// 整数を扱う命令か（インタープリターの is_int と同じ）
fn is_int(m: M) -> bool {
    use M::*;
    matches!(m, Fild | Fist | Fistp | Fisttp | Fiadd | Fisub | Fisubr | Fimul | Fidiv | Fidivr | Ficom | Ficomp)
}

fn mem_op(i: &Instruction) -> bool {
    i.op_count() > 0 && i.op_kind(0) == OpKind::Memory
}

/// st(n) のレジスタ番号
fn streg(r: Register) -> Option<i32> {
    (Register::ST0..=Register::ST7).contains(&r).then(|| r as i32 - Register::ST0 as i32)
}

/// 変換できるか（できるならフラグの書き読み）。80 ビットのメモリ、ax 以外への fnstsw は扱わない
pub(super) fn rw(i: &Instruction) -> Option<Rw> {
    let m = i.mnemonic();
    if mem_op(i) {
        let Opnd::Mem(_, size) = operand(i, 0)? else { return None };
        let ok = if is_int(m) { matches!(size, 2 | 4 | 8) } else if matches!(m, M::Fnstsw | M::Fnstcw | M::Fldcw) { size == 2 } else { matches!(size, 4 | 8) };
        if !ok {
            return None;
        }
    } else {
        for n in 0..i.op_count() {
            if i.op_kind(n) != OpKind::Register {
                return None;
            }
            let r = i.op_register(n);
            if streg(r).is_none() && !(m == M::Fnstsw && r == Register::AX) {
                return None;
            }
        }
    }
    Some(match m {
        M::Fcomi | M::Fcomip | M::Fucomi | M::Fucomip => Rw { kill: FALL, emit: FALL, read: 0 },
        _ => Rw { kill: 0, emit: 0, read: 0 },
    })
}

impl Asm {
    /// st(n)（今のずれから見た）の f64 の入れ物のホスト番地（i32）を積む。読み書きの MemArg の offset に fpu の位置を入れる
    fn st_addr(&mut self, n: i32) {
        self.cpu();
        self.body.push(W::LocalGet(FT));
        self.body.push(W::I32Const(self.fdelta + n));
        self.body.push(W::I32Add);
        self.body.push(W::I32Const(7));
        self.body.push(W::I32And);
        self.body.push(W::I32Const(3));
        self.body.push(W::I32Shl);
        self.body.push(W::I32Add);
    }
    fn st_load(&mut self, n: i32) {
        self.st_addr(n);
        self.body.push(W::F64Load(fpu_arg()));
    }
    /// st(n) に D0 を書く
    fn st_store_d0(&mut self, n: i32) {
        self.st_addr(n);
        self.body.push(W::LocalGet(D0));
        self.body.push(W::F64Store(fpu_arg()));
    }
    /// D0 を積む（先頭を 1 つ下げて st(0) に書く）
    fn fpush_d0(&mut self) {
        self.st_store_d0(-1);
        self.fdelta -= 1;
    }
    fn fpop(&mut self) {
        self.fdelta += 1;
    }
    /// メモリの引数を f64 で積む（整数の命令なら整数として読む）。読めなければ中断（その前に何も変えていないこと）
    fn mem_f64(&mut self, i: &Instruction, insn_addr: u64, count: u32) -> Option<()> {
        let Opnd::Mem(mo, size) = operand(i, 0)? else { return None };
        self.calc_addr(&mo);
        self.resolve_ptr(size, insn_addr, count);
        self.body.push(W::LocalGet(HP));
        let a = |align| MemArg { offset: 0, align, memory_index: 0 };
        match (is_int(i.mnemonic()), size) {
            (true, 2) => self.body.extend([W::I32Load16S(a(0)), W::F64ConvertI32S]),
            (true, 4) => self.body.extend([W::I32Load(a(0)), W::F64ConvertI32S]),
            (true, _) => self.body.extend([W::I64Load(a(0)), W::F64ConvertI64S]),
            (false, 4) => self.body.extend([W::F32Load(a(0)), W::F64PromoteF32]),
            (false, _) => self.body.push(W::F64Load(a(0))),
        }
        Some(())
    }
    /// D0 を制御ワードの丸めの向きで丸めて積む（00 最近接偶数、01 切り下げ、10 切り上げ、11 切り捨て）
    fn fround_d0(&mut self) {
        self.cpu();
        self.body.push(W::I32Load16U(MemArg { offset: offset_of!(Cpu, fpu_cw) as u64, align: 1, memory_index: 0 }));
        self.body.push(W::I32Const(10));
        self.body.push(W::I32ShrU);
        self.body.push(W::I32Const(3));
        self.body.push(W::I32And);
        self.body.push(W::LocalSet(T0));
        let mode = |b: &mut Vec<W<'static>>, k: i32| {
            b.push(W::LocalGet(T0));
            b.push(W::I32Const(k));
            b.push(W::I32Eq);
        };
        self.body.extend([W::LocalGet(D0), W::F64Nearest, W::LocalGet(D0), W::F64Floor, W::LocalGet(D0), W::F64Ceil, W::LocalGet(D0), W::F64Trunc]);
        mode(&mut self.body, 2);
        self.body.push(W::Select);
        mode(&mut self.body, 1);
        self.body.push(W::Select);
        mode(&mut self.body, 0);
        self.body.push(W::Select);
    }
    /// fcom の結果（C0、C2、C3。C1 は 0）を a=D0、b=D1 から fpu_c に書く
    fn fcom_d0_d1(&mut self) {
        let c = [offset_of!(Cpu, fpu_c.0) as u64, offset_of!(Cpu, fpu_c.1) as u64, offset_of!(Cpu, fpu_c.2) as u64, offset_of!(Cpu, fpu_c.3) as u64];
        // nan = a != a | b != b
        self.body.extend([W::LocalGet(D0), W::LocalGet(D0), W::F64Ne, W::LocalGet(D1), W::LocalGet(D1), W::F64Ne, W::I32Or, W::LocalSet(T0)]);
        let st = |o| W::I32Store8(MemArg { offset: o, align: 0, memory_index: 0 });
        // C0 = nan | a < b
        self.cpu();
        self.body.extend([W::LocalGet(T0), W::LocalGet(D0), W::LocalGet(D1), W::F64Lt, W::I32Or, st(c[0])]);
        self.cpu();
        self.body.extend([W::I32Const(0), st(c[1])]);
        self.cpu();
        self.body.extend([W::LocalGet(T0), st(c[2])]);
        // C3 = nan | a == b
        self.cpu();
        self.body.extend([W::LocalGet(T0), W::LocalGet(D0), W::LocalGet(D1), W::F64Eq, W::I32Or, st(c[3])]);
    }
}

fn fpu_arg() -> MemArg {
    MemArg { offset: offset_of!(Cpu, fpu) as u64, align: 3, memory_index: 0 }
}

/// 1 命令を変換する
pub(super) fn emit(asm: &mut Asm, i: &Instruction, insn_addr: u64, count: u32) -> Option<()> {
    let m = i.mnemonic();
    let mem = mem_op(i);
    let reg = |n: u32| streg(i.op_register(n));
    match m {
        M::Fld | M::Fild => {
            if mem {
                asm.mem_f64(i, insn_addr, count)?;
            } else {
                asm.st_load(reg(0)?);
            }
            asm.body.push(W::LocalSet(D0));
            asm.fpush_d0();
        }
        M::Fld1 | M::Fldz | M::Fldpi | M::Fldl2e | M::Fldl2t | M::Fldlg2 | M::Fldln2 => {
            use std::f64::consts::*;
            let v = match m {
                M::Fld1 => 1.0,
                M::Fldz => 0.0,
                M::Fldpi => PI,
                M::Fldl2e => LOG2_E,
                M::Fldl2t => LOG2_10,
                M::Fldlg2 => LOG10_2,
                _ => LN_2,
            };
            asm.body.push(W::F64Const(v.into()));
            asm.body.push(W::LocalSet(D0));
            asm.fpush_d0();
        }
        M::Fst | M::Fstp | M::Fist | M::Fistp | M::Fisttp => {
            asm.st_load(0);
            asm.body.push(W::LocalSet(D0));
            if mem {
                let Opnd::Mem(mo, size) = operand(i, 0)? else { return None };
                asm.calc_addr(&mo);
                asm.resolve_ptr_w(size, insn_addr, count);
                asm.body.push(W::LocalGet(HP));
                let a = |align| MemArg { offset: 0, align, memory_index: 0 };
                if is_int(m) {
                    // fisttp は 0 方向への切り捨て、fist、fistp は丸め。i64 に飽和させてから size バイトを書く（Rust の as と同じ）
                    if m == M::Fisttp {
                        asm.body.extend([W::LocalGet(D0), W::F64Trunc]);
                    } else {
                        asm.fround_d0();
                    }
                    asm.body.push(W::I64TruncSatF64S);
                    asm.body.push(match size {
                        2 => W::I64Store16(a(0)),
                        4 => W::I64Store32(a(0)),
                        _ => W::I64Store(a(0)),
                    });
                } else if size == 4 {
                    asm.body.extend([W::LocalGet(D0), W::F32DemoteF64, W::F32Store(a(0))]);
                } else {
                    asm.body.extend([W::LocalGet(D0), W::F64Store(a(0))]);
                }
            } else {
                asm.st_store_d0(reg(0)?);
            }
            if matches!(m, M::Fstp | M::Fistp | M::Fisttp) {
                asm.fpop();
            }
        }
        M::Fadd | M::Fsub | M::Fsubr | M::Fmul | M::Fdiv | M::Fdivr | M::Faddp | M::Fsubp | M::Fsubrp | M::Fmulp | M::Fdivp | M::Fdivrp | M::Fiadd | M::Fisub | M::Fisubr | M::Fimul | M::Fidiv | M::Fidivr => {
            // a = 宛先、b = もう一方（メモリなら st(0) と メモリ）
            let p = matches!(m, M::Faddp | M::Fsubp | M::Fsubrp | M::Fmulp | M::Fdivp | M::Fdivrp);
            let (dst, src) = if mem {
                (0, None)
            } else if i.op_count() == 2 {
                (reg(0)?, Some(reg(1)?))
            } else if p {
                (1, Some(0))
            } else {
                (0, Some(1))
            };
            // メモリは先に読む（中断しても何も変えていないように）
            match src {
                None => asm.mem_f64(i, insn_addr, count)?,
                Some(s) => asm.st_load(s),
            }
            asm.body.push(W::LocalSet(D1));
            asm.st_load(dst);
            asm.body.push(W::LocalSet(D0));
            let (x, y) = if matches!(m, M::Fsubr | M::Fsubrp | M::Fisubr | M::Fdivr | M::Fdivrp | M::Fidivr) { (D1, D0) } else { (D0, D1) };
            asm.body.push(W::LocalGet(x));
            asm.body.push(W::LocalGet(y));
            asm.body.push(match m {
                M::Fadd | M::Faddp | M::Fiadd => W::F64Add,
                M::Fmul | M::Fmulp | M::Fimul => W::F64Mul,
                M::Fsub | M::Fsubp | M::Fisub | M::Fsubr | M::Fsubrp | M::Fisubr => W::F64Sub,
                _ => W::F64Div,
            });
            asm.body.push(W::LocalSet(D0));
            asm.st_store_d0(dst);
            if p {
                asm.fpop();
            }
        }
        M::Fchs | M::Fabs | M::Fsqrt | M::Frndint => {
            asm.st_load(0);
            asm.body.push(W::LocalSet(D0));
            match m {
                M::Fchs => asm.body.extend([W::LocalGet(D0), W::F64Neg]),
                M::Fabs => asm.body.extend([W::LocalGet(D0), W::F64Abs]),
                M::Fsqrt => asm.body.extend([W::LocalGet(D0), W::F64Sqrt]),
                _ => asm.fround_d0(),
            }
            asm.body.push(W::LocalSet(D0));
            asm.st_store_d0(0);
        }
        M::Fxch => {
            let n = if i.op_count() > 1 { reg(1)? } else if i.op_count() == 1 { reg(0)? } else { 1 };
            asm.st_load(0);
            asm.body.push(W::LocalSet(D1));
            asm.st_load(n);
            asm.body.push(W::LocalSet(D0));
            asm.st_store_d0(0);
            asm.body.push(W::LocalGet(D1));
            asm.body.push(W::LocalSet(D0));
            asm.st_store_d0(n);
        }
        M::Fcom | M::Fcomp | M::Fcompp | M::Fucom | M::Fucomp | M::Fucompp | M::Ficom | M::Ficomp => {
            if mem {
                asm.mem_f64(i, insn_addr, count)?;
            } else if i.op_count() > 0 {
                asm.st_load(reg(i.op_count() - 1)?);
            } else {
                asm.st_load(1);
            }
            asm.body.push(W::LocalSet(D1));
            asm.st_load(0);
            asm.body.push(W::LocalSet(D0));
            asm.fcom_d0_d1();
            match m {
                M::Fcomp | M::Fucomp | M::Ficomp => asm.fpop(),
                M::Fcompp | M::Fucompp => {
                    asm.fpop();
                    asm.fpop();
                }
                _ => {}
            }
        }
        // 結果を EFLAGS に（順序づけできなければ zf=pf=cf=1）。of と sf は 0
        M::Fcomi | M::Fcomip | M::Fucomi | M::Fucomip => {
            asm.st_load(reg(if i.op_count() > 1 { 1 } else { 0 })?);
            asm.body.push(W::LocalSet(D1));
            asm.st_load(0);
            asm.body.push(W::LocalSet(D0));
            asm.body.extend([W::LocalGet(D0), W::LocalGet(D0), W::F64Ne, W::LocalGet(D1), W::LocalGet(D1), W::F64Ne, W::I32Or, W::LocalSet(T0)]);
            let st = |o| W::I32Store8(MemArg { offset: o, align: 0, memory_index: 0 });
            let o = (asm.off.zf, asm.off.pf, asm.off.cf, asm.off.of, asm.off.sf);
            if asm.flag_mask & FZF != 0 {
                asm.cpu();
                asm.body.extend([W::LocalGet(T0), W::LocalGet(D0), W::LocalGet(D1), W::F64Eq, W::I32Or, st(o.0)]);
            }
            if asm.flag_mask & FPF != 0 {
                asm.cpu();
                asm.body.extend([W::LocalGet(T0), st(o.1)]);
            }
            if asm.flag_mask & FCF != 0 {
                asm.cpu();
                asm.body.extend([W::LocalGet(T0), W::LocalGet(D0), W::LocalGet(D1), W::F64Lt, W::I32Or, st(o.2)]);
            }
            for (bit, off) in [(FOF, o.3), (FSF, o.4)] {
                if asm.flag_mask & bit != 0 {
                    asm.cpu();
                    asm.body.extend([W::I32Const(0), st(off)]);
                }
            }
            if matches!(m, M::Fcomip | M::Fucomip) {
                asm.fpop();
            }
        }
        // 状態ワード: C0 bit8、C1 bit9、C2 bit10、先頭 bit11〜13、C3 bit14
        M::Fnstsw => {
            let c = [offset_of!(Cpu, fpu_c.0), offset_of!(Cpu, fpu_c.1), offset_of!(Cpu, fpu_c.2)];
            let ld = |o: usize| W::I32Load8U(MemArg { offset: o as u64, align: 0, memory_index: 0 });
            if mem {
                let Opnd::Mem(mo, _) = operand(i, 0)? else { return None };
                asm.calc_addr(&mo);
                asm.resolve_ptr_w(2, insn_addr, count);
                asm.body.push(W::LocalGet(HP));
            }
            for (k, o) in c.iter().enumerate() {
                asm.cpu();
                asm.body.extend([ld(*o), W::I32Const(8 + k as i32), W::I32Shl]);
                if k > 0 {
                    asm.body.push(W::I32Or);
                }
            }
            asm.body.extend([W::LocalGet(FT), W::I32Const(asm.fdelta), W::I32Add, W::I32Const(7), W::I32And, W::I32Const(11), W::I32Shl, W::I32Or]);
            asm.cpu();
            asm.body.extend([ld(offset_of!(Cpu, fpu_c.3)), W::I32Const(14), W::I32Shl, W::I32Or]);
            if mem {
                asm.body.push(W::I32Store16(MemArg { offset: 0, align: 0, memory_index: 0 }));
            } else {
                asm.body.push(W::I64ExtendI32U);
                asm.store_reg(0, 2);
            }
        }
        // 制御ワード（丸めの設定。fround_d0 が読む）
        M::Fnstcw | M::Fldcw => {
            let Opnd::Mem(mo, _) = operand(i, 0)? else { return None };
            let cw = MemArg { offset: offset_of!(Cpu, fpu_cw) as u64, align: 1, memory_index: 0 };
            let at = MemArg { offset: 0, align: 0, memory_index: 0 };
            asm.calc_addr(&mo);
            if m == M::Fnstcw {
                asm.resolve_ptr_w(2, insn_addr, count);
                asm.body.push(W::LocalGet(HP));
                asm.cpu();
                asm.body.extend([W::I32Load16U(cw), W::I32Store16(at)]);
            } else {
                asm.resolve_ptr(2, insn_addr, count);
                asm.cpu();
                asm.body.extend([W::LocalGet(HP), W::I32Load16U(at), W::I32Store16(cw)]);
            }
        }
        M::Fnclex | M::Wait => {}
        // 超越関数などは Rust（インタープリターと同じ計算）を呼ぶ。先頭とずれを Cpu に書き戻してから呼び、先頭を読み直す
        _ if crate::x87::TRANSCEND.contains(&m) => {
            let k = crate::x87::TRANSCEND.iter().position(|&t| t == m)?;
            let fdelta = asm.fdelta;
            asm.fpu_sync();
            asm.body.extend([W::LocalGet(FT), W::I32Const(fdelta), W::I32Add, W::I32Const(7), W::I32And, W::LocalSet(FT)]);
            asm.fdelta = 0;
            asm.cpu();
            asm.body.extend([W::I32Const(k as i32), W::I32Const(crate::jit::x87_helper as usize as i32), W::CallIndirect { type_index: 1, table_index: 0 }]);
            asm.cpu();
            asm.body.extend([W::I32Load(MemArg { offset: offset_of!(Cpu, fpu_top) as u64, align: 2, memory_index: 0 }), W::LocalSet(FT)]);
        }
        _ => return None,
    }
    Some(())
}
