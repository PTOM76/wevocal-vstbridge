//! x86 の基本ブロックを wasm の関数に変換する（memo/jit.md の段階 1。整数、分岐、メモリ）。
//! 生成する関数は fn(cpu: i32) -> i32。cpu は Cpu の番地、戻り値は実行した命令の数。
//! 対応しない命令やメモリの遅い道に当たったら、その命令の番地を rip に書き、cpu.jit_bail=1 にして戻る（インタープリターが続ける）。
//! フラグを厳密に合わせられる命令だけを変換の対象にし、それ以外はブロックをその手前で区切る（残りはインタープリター）。
use crate::cpu::{Cpu, Flags};
use crate::mem::Memory;
use iced_x86::{Code, ConditionCode, Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic as M, OpKind, Register};
use std::mem::offset_of;
use wasm_encoder::{BlockType, Instruction as W, MemArg, ValType};

/// 局所変数の番号（0 は引数の cpu）
const CPU: u32 = 0;
const A: u32 = 1; // i64 作業用（引数 0）
const B: u32 = 2; // i64 作業用（引数 1）
const R: u32 = 3; // i64 作業用（結果）
const GA: u32 = 4; // i32 ゲストの番地
const HP: u32 = 5; // i32 ホストの番地

fn off_reg(idx: usize) -> u64 {
    (offset_of!(Cpu, regs) + idx * 8) as u64
}
fn off_flag(f: u64) -> u64 {
    offset_of!(Cpu, flags) as u64 + f
}
struct Off {
    rip: u64,
    bail: u64,
    cf: u64,
    zf: u64,
    sf: u64,
    of: u64,
    pf: u64,
}
fn offs() -> Off {
    Off {
        rip: offset_of!(Cpu, rip) as u64,
        bail: offset_of!(Cpu, jit_bail) as u64,
        cf: off_flag(offset_of!(Flags, cf) as u64),
        zf: off_flag(offset_of!(Flags, zf) as u64),
        sf: off_flag(offset_of!(Flags, sf) as u64),
        of: off_flag(offset_of!(Flags, of) as u64),
        pf: off_flag(offset_of!(Flags, pf) as u64),
    }
}

/// レジスタ（汎用）の番号と大きさ。AH/BH などの上位バイトは None（対応しない）
fn gpr(r: Register) -> Option<(usize, usize)> {
    if matches!(r, Register::AH | Register::CH | Register::DH | Register::BH) {
        return None;
    }
    let full = r.full_register();
    if !(Register::RAX..=Register::R15).contains(&full) {
        return None;
    }
    Some((full as usize - Register::RAX as usize, r.size()))
}

/// メモリの引数（基点 + 添字 × 倍率 + 変位）。セグメントは無し・ds・ss・es・cs だけ（fs/gs は対応しない）
struct Mem {
    base: Option<usize>,
    index: Option<usize>,
    scale: u32,
    disp: u32,
}

enum Opnd {
    Reg(usize, usize),
    Mem(Mem, usize),
    Imm(u64),
}

struct Asm {
    body: Vec<W<'static>>,
    off: Off,
    pages_ptr: u32,
    bits: u32,
}

impl Asm {
    /// cpu の番地（局所 0）を積む
    fn cpu(&mut self) {
        self.body.push(W::LocalGet(CPU));
    }
    /// レジスタ idx の 64 ビットを積む
    fn load_reg(&mut self, idx: usize) {
        self.cpu();
        self.body.push(W::I64Load(MemArg { offset: off_reg(idx), align: 3, memory_index: 0 }));
    }
    /// レジスタ idx に、スタックの上の i64 を size バイトとして書く（x86-64 の決まり）
    fn store_reg(&mut self, idx: usize, size: usize) {
        // 値はスタックにある。いったん R に退避してから組み立てる
        self.body.push(W::LocalSet(R));
        self.cpu();
        match size {
            8 => self.body.push(W::LocalGet(R)),
            4 => {
                self.body.push(W::LocalGet(R));
                self.body.push(W::I64Const(0xffff_ffff));
                self.body.push(W::I64And);
            }
            s => {
                let m = (1u64 << (s * 8)) - 1;
                self.load_reg(idx);
                self.body.push(W::I64Const(!m as i64));
                self.body.push(W::I64And);
                self.body.push(W::LocalGet(R));
                self.body.push(W::I64Const(m as i64));
                self.body.push(W::I64And);
                self.body.push(W::I64Or);
            }
        }
        self.body.push(W::I64Store(MemArg { offset: off_reg(idx), align: 3, memory_index: 0 }));
    }
    /// ゲストの番地（i32）を GA に計算して積む
    fn calc_addr(&mut self, mo: &Mem) {
        self.body.push(W::I32Const(mo.disp as i32));
        if let Some(b) = mo.base {
            self.load_reg(b);
            self.body.push(W::I32WrapI64);
            self.body.push(W::I32Add);
        }
        if let Some(ix) = mo.index {
            self.load_reg(ix);
            self.body.push(W::I32WrapI64);
            if mo.scale > 1 {
                self.body.push(W::I32Const(mo.scale as i32));
                self.body.push(W::I32Mul);
            }
            self.body.push(W::I32Add);
        }
        self.body.push(W::LocalSet(GA));
    }
    /// GA のページを引いてホストの番地を HP に入れる。引けなければ中断（bail）。insn_addr と count は中断時に使う
    fn resolve_ptr(&mut self, size: usize, insn_addr: u64, count: u32) {
        // page = load32(pages_ptr + (GA>>12)*4)
        self.body.push(W::LocalGet(GA));
        self.body.push(W::I32Const(12));
        self.body.push(W::I32ShrU);
        self.body.push(W::I32Const(4));
        self.body.push(W::I32Mul);
        self.body.push(W::I32Const(self.pages_ptr as i32));
        self.body.push(W::I32Add);
        self.body.push(W::I32Load(MemArg { offset: 0, align: 2, memory_index: 0 }));
        self.body.push(W::LocalTee(HP));
        // page==0 か、ページの終わりをまたぐか
        self.body.push(W::I32Eqz);
        self.body.push(W::LocalGet(GA));
        self.body.push(W::I32Const(0xfff));
        self.body.push(W::I32And);
        self.body.push(W::I32Const(size as i32));
        self.body.push(W::I32Add);
        self.body.push(W::I32Const(4096));
        self.body.push(W::I32GtU);
        self.body.push(W::I32Or);
        self.body.push(W::If(BlockType::Empty));
        self.emit_bail(insn_addr, count);
        self.body.push(W::End);
        // HP = HP + (GA & 0xfff)
        self.body.push(W::LocalGet(HP));
        self.body.push(W::LocalGet(GA));
        self.body.push(W::I32Const(0xfff));
        self.body.push(W::I32And);
        self.body.push(W::I32Add);
        self.body.push(W::LocalSet(HP));
    }
    /// rip を v にして、count を積んで返す
    fn emit_exit(&mut self, v: u64, count: u32) {
        self.cpu();
        self.body.push(W::I64Const(v as i64));
        self.body.push(W::I64Store(MemArg { offset: self.off.rip, align: 3, memory_index: 0 }));
        self.body.push(W::I32Const(count as i32));
        self.body.push(W::Return);
    }
    /// rip を命令の番地にし、bail=1 にして count を返す
    fn emit_bail(&mut self, insn_addr: u64, count: u32) {
        self.cpu();
        self.body.push(W::I64Const(insn_addr as i64));
        self.body.push(W::I64Store(MemArg { offset: self.off.rip, align: 3, memory_index: 0 }));
        self.cpu();
        self.body.push(W::I32Const(1));
        self.body.push(W::I32Store(MemArg { offset: self.off.bail, align: 2, memory_index: 0 }));
        self.body.push(W::I32Const(count as i32));
        self.body.push(W::Return);
    }
    /// 引数の値（i64、size で 0 拡張）を積む
    fn push_operand(&mut self, op: &Opnd, insn_addr: u64, count: u32) {
        match *op {
            Opnd::Reg(idx, size) => {
                self.load_reg(idx);
                mask_i64(&mut self.body, size);
            }
            Opnd::Imm(v) => self.body.push(W::I64Const(v as i64)),
            Opnd::Mem(ref mo, size) => {
                self.calc_addr(mo);
                self.resolve_ptr(size, insn_addr, count);
                self.body.push(W::LocalGet(HP));
                self.body.push(load_op(size));
            }
        }
    }
    /// スタックの上の i64 を引数へ書く（size バイト）。メモリのときは addr を先に計算しておく必要があるので、store 専用に calc と resolve を内側で行う版は別に用意する
    fn store_operand_reg(&mut self, idx: usize, size: usize) {
        self.store_reg(idx, size);
    }
}

/// i64 を size バイトに 0 拡張（マスク）する
fn mask_i64(b: &mut Vec<W<'static>>, size: usize) {
    if size < 8 {
        b.push(W::I64Const(((1u64 << (size * 8)) - 1) as i64));
        b.push(W::I64And);
    }
}
fn load_op(size: usize) -> W<'static> {
    match size {
        1 => W::I64Load8U(MemArg { offset: 0, align: 0, memory_index: 0 }),
        2 => W::I64Load16U(MemArg { offset: 0, align: 1, memory_index: 0 }),
        4 => W::I64Load32U(MemArg { offset: 0, align: 2, memory_index: 0 }),
        _ => W::I64Load(MemArg { offset: 0, align: 3, memory_index: 0 }),
    }
}
fn store_op(size: usize) -> W<'static> {
    match size {
        1 => W::I64Store8(MemArg { offset: 0, align: 0, memory_index: 0 }),
        2 => W::I64Store16(MemArg { offset: 0, align: 1, memory_index: 0 }),
        4 => W::I64Store32(MemArg { offset: 0, align: 2, memory_index: 0 }),
        _ => W::I64Store(MemArg { offset: 0, align: 3, memory_index: 0 }),
    }
}

fn operand(i: &Instruction, n: u32) -> Option<Opnd> {
    match i.op_kind(n) {
        OpKind::Register => {
            let (idx, size) = gpr(i.op_register(n))?;
            Some(Opnd::Reg(idx, size))
        }
        OpKind::Memory => {
            // rip 相対や特殊は後回し（対応しない）
            let base = i.memory_base();
            let index = i.memory_index();
            if base == Register::RIP {
                return None;
            }
            match i.segment_prefix() {
                Register::None | Register::DS | Register::SS | Register::ES | Register::CS => {}
                _ => return None,
            }
            let b = if base == Register::None { None } else { Some(gpr(base)?.0) };
            let ix = if index == Register::None { None } else { Some(gpr(index)?.0) };
            Some(Opnd::Mem(Mem { base: b, index: ix, scale: i.memory_index_scale(), disp: i.memory_displacement64() as u32 }, i.memory_size().size()))
        }
        OpKind::Immediate8 => Some(Opnd::Imm(i.immediate8() as u64)),
        OpKind::Immediate8to16 => Some(Opnd::Imm(i.immediate8to16() as u64)),
        OpKind::Immediate8to32 => Some(Opnd::Imm(i.immediate8to32() as u64)),
        OpKind::Immediate8to64 => Some(Opnd::Imm(i.immediate8to64() as u64)),
        OpKind::Immediate16 => Some(Opnd::Imm(i.immediate16() as u64)),
        OpKind::Immediate32 => Some(Opnd::Imm(i.immediate32() as u64)),
        OpKind::Immediate32to64 => Some(Opnd::Imm(i.immediate32to64() as u64)),
        OpKind::Immediate64 => Some(Opnd::Imm(i.immediate64())),
        _ => None,
    }
}

fn dst_size(i: &Instruction) -> usize {
    match i.op_kind(0) {
        OpKind::Register => i.op_register(0).size(),
        OpKind::Memory => i.memory_size().size(),
        _ => 8,
    }
}

/// 符号ビット（0/1、i32）を R（結果 i64）から。size の最上位ビット
fn sign_bit(b: &mut Vec<W<'static>>, local: u32, size: usize) {
    b.push(W::LocalGet(local));
    b.push(W::I64Const((size as i64) * 8 - 1));
    b.push(W::I64ShrU);
    b.push(W::I64Const(1));
    b.push(W::I64And);
    b.push(W::I32WrapI64);
}

impl Asm {
    /// A, B（引数、マスク済み）と R（結果、マスク済み）から add/sub のフラグを書く
    fn flags_addsub(&mut self, sub: bool, size: usize) {
        let o = &self.off;
        let (cf, zf, sf, of, pf) = (o.cf, o.zf, o.sf, o.of, o.pf);
        // zf
        self.cpu();
        self.body.push(W::LocalGet(R));
        self.body.push(W::I64Eqz);
        self.body.push(W::I32Store8(MemArg { offset: zf, align: 0, memory_index: 0 }));
        // sf
        self.cpu();
        sign_bit(&mut self.body, R, size);
        self.body.push(W::I32Store8(MemArg { offset: sf, align: 0, memory_index: 0 }));
        // pf = popcnt(R & 0xff) & 1 == 0
        self.cpu();
        self.body.push(W::LocalGet(R));
        self.body.push(W::I32WrapI64);
        self.body.push(W::I32Const(0xff));
        self.body.push(W::I32And);
        self.body.push(W::I32Popcnt);
        self.body.push(W::I32Const(1));
        self.body.push(W::I32And);
        self.body.push(W::I32Eqz);
        self.body.push(W::I32Store8(MemArg { offset: pf, align: 0, memory_index: 0 }));
        // cf
        self.cpu();
        if sub {
            // a < b
            self.body.push(W::LocalGet(A));
            self.body.push(W::LocalGet(B));
            self.body.push(W::I64LtU);
        } else if size == 8 {
            // (a+b) < a  （R はマスク済みで size8 ではそのまま）
            self.body.push(W::LocalGet(R));
            self.body.push(W::LocalGet(A));
            self.body.push(W::I64LtU);
        } else {
            // a+b > mask  → (a+b) をマスク前で作る
            self.body.push(W::LocalGet(A));
            self.body.push(W::LocalGet(B));
            self.body.push(W::I64Add);
            self.body.push(W::I64Const(((1u64 << (size * 8)) - 1) as i64));
            self.body.push(W::I64GtU);
        }
        self.body.push(W::I32Store8(MemArg { offset: cf, align: 0, memory_index: 0 }));
        // of
        self.cpu();
        // sa=sign(A), sb=sign(B), sr=sign(R)
        sign_bit(&mut self.body, A, size); // sa
        sign_bit(&mut self.body, B, size); // sb
        if sub {
            self.body.push(W::I32Ne); // sa!=sb
        } else {
            self.body.push(W::I32Eq); // sa==sb
        }
        sign_bit(&mut self.body, R, size); // sr
        sign_bit(&mut self.body, A, size); // sa
        self.body.push(W::I32Ne); // sr!=sa
        self.body.push(W::I32And);
        self.body.push(W::I32Store8(MemArg { offset: of, align: 0, memory_index: 0 }));
    }
    /// R（マスク済み）から logic（and/or/xor/test）のフラグ（cf=0, of=0, zf, sf, pf）
    fn flags_logic(&mut self, size: usize) {
        let o = &self.off;
        let (cf, zf, sf, of, pf) = (o.cf, o.zf, o.sf, o.of, o.pf);
        self.cpu();
        self.body.push(W::I32Const(0));
        self.body.push(W::I32Store8(MemArg { offset: cf, align: 0, memory_index: 0 }));
        self.cpu();
        self.body.push(W::I32Const(0));
        self.body.push(W::I32Store8(MemArg { offset: of, align: 0, memory_index: 0 }));
        self.cpu();
        self.body.push(W::LocalGet(R));
        self.body.push(W::I64Eqz);
        self.body.push(W::I32Store8(MemArg { offset: zf, align: 0, memory_index: 0 }));
        self.cpu();
        sign_bit(&mut self.body, R, size);
        self.body.push(W::I32Store8(MemArg { offset: sf, align: 0, memory_index: 0 }));
        self.cpu();
        self.body.push(W::LocalGet(R));
        self.body.push(W::I32WrapI64);
        self.body.push(W::I32Const(0xff));
        self.body.push(W::I32And);
        self.body.push(W::I32Popcnt);
        self.body.push(W::I32Const(1));
        self.body.push(W::I32And);
        self.body.push(W::I32Eqz);
        self.body.push(W::I32Store8(MemArg { offset: pf, align: 0, memory_index: 0 }));
    }
    /// 条件コードを i32（0/1）で積む
    fn push_cond(&mut self, c: ConditionCode) {
        let o = &self.off;
        let load = |b: &mut Vec<W<'static>>, off: u64| {
            b.push(W::LocalGet(CPU));
            b.push(W::I32Load8U(MemArg { offset: off, align: 0, memory_index: 0 }));
        };
        match c {
            ConditionCode::o => load(&mut self.body, o.of),
            ConditionCode::no => {
                load(&mut self.body, o.of);
                self.body.push(W::I32Eqz);
            }
            ConditionCode::b => load(&mut self.body, o.cf),
            ConditionCode::ae => {
                load(&mut self.body, o.cf);
                self.body.push(W::I32Eqz);
            }
            ConditionCode::e => load(&mut self.body, o.zf),
            ConditionCode::ne => {
                load(&mut self.body, o.zf);
                self.body.push(W::I32Eqz);
            }
            ConditionCode::be => {
                load(&mut self.body, o.cf);
                load(&mut self.body, o.zf);
                self.body.push(W::I32Or);
            }
            ConditionCode::a => {
                load(&mut self.body, o.cf);
                load(&mut self.body, o.zf);
                self.body.push(W::I32Or);
                self.body.push(W::I32Eqz);
            }
            ConditionCode::s => load(&mut self.body, o.sf),
            ConditionCode::ns => {
                load(&mut self.body, o.sf);
                self.body.push(W::I32Eqz);
            }
            ConditionCode::p => load(&mut self.body, o.pf),
            ConditionCode::np => {
                load(&mut self.body, o.pf);
                self.body.push(W::I32Eqz);
            }
            ConditionCode::l => {
                load(&mut self.body, o.sf);
                load(&mut self.body, o.of);
                self.body.push(W::I32Ne);
            }
            ConditionCode::ge => {
                load(&mut self.body, o.sf);
                load(&mut self.body, o.of);
                self.body.push(W::I32Eq);
            }
            ConditionCode::le => {
                load(&mut self.body, o.zf);
                load(&mut self.body, o.sf);
                load(&mut self.body, o.of);
                self.body.push(W::I32Ne);
                self.body.push(W::I32Or);
            }
            ConditionCode::g => {
                load(&mut self.body, o.zf);
                load(&mut self.body, o.sf);
                load(&mut self.body, o.of);
                self.body.push(W::I32Ne);
                self.body.push(W::I32Or);
                self.body.push(W::I32Eqz);
            }
            ConditionCode::None => self.body.push(W::I32Const(1)),
        }
    }
}

/// 引数 0 に、スタックの上の i64 を書く（レジスタかメモリ）。メモリのときは addr を計算して resolve 済みの HP に書く。
/// 値はスタックの上にある前提
fn emit_store_dst(asm: &mut Asm, i: &Instruction, size: usize, insn_addr: u64, count: u32) -> Option<()> {
    match i.op_kind(0) {
        OpKind::Register => {
            let (idx, _) = gpr(i.op_register(0))?;
            asm.store_operand_reg(idx, size);
        }
        OpKind::Memory => {
            // 値を R に退避し、addr を計算して resolve、HP と値を積んで store
            asm.body.push(W::LocalSet(R));
            let Opnd::Mem(mo, _) = operand(i, 0)? else { return None };
            asm.calc_addr(&mo);
            asm.resolve_ptr(size, insn_addr, count);
            asm.body.push(W::LocalGet(HP));
            asm.body.push(W::LocalGet(R));
            asm.body.push(store_op(size));
        }
        _ => return None,
    }
    Some(())
}

/// 1 命令を変換する。対応しないなら None（呼び出し側がブロックをその手前で切る）
fn emit_insn(asm: &mut Asm, i: &Instruction, insn_addr: u64, count: u32) -> Option<()> {
    let m = i.mnemonic();
    match m {
        M::Nop | M::Endbr64 => Some(()),
        M::Mov => {
            let src = operand(i, 1)?;
            // レジスタ↔メモリ、即値。dst はレジスタかメモリ
            let size = dst_size(i);
            asm.push_operand(&src, insn_addr, count);
            emit_store_dst(asm, i, size, insn_addr, count)
        }
        M::Movzx => {
            let src = operand(i, 1)?;
            let ssize = src_size(i);
            asm.push_operand(&src, insn_addr, count);
            mask_i64(&mut asm.body, ssize);
            let (idx, _) = gpr(i.op_register(0))?;
            asm.store_reg(idx, i.op_register(0).size());
            Some(())
        }
        M::Movsx | M::Movsxd => {
            let src = operand(i, 1)?;
            let ssize = src_size(i);
            asm.push_operand(&src, insn_addr, count);
            // 符号拡張
            match ssize {
                1 => asm.body.push(W::I64Extend8S),
                2 => asm.body.push(W::I64Extend16S),
                4 => asm.body.push(W::I64Extend32S),
                _ => {}
            }
            let (idx, _) = gpr(i.op_register(0))?;
            asm.store_reg(idx, i.op_register(0).size());
            Some(())
        }
        M::Lea => {
            let Opnd::Mem(mo, _) = operand(i, 1)? else { return None };
            asm.calc_addr(&mo);
            asm.body.push(W::LocalGet(GA));
            asm.body.push(W::I64ExtendI32U);
            let (idx, _) = gpr(i.op_register(0))?;
            asm.store_reg(idx, i.op_register(0).size());
            Some(())
        }
        M::Add | M::Sub | M::Cmp | M::And | M::Or | M::Xor | M::Test => {
            let size = dst_size(i);
            let dst = operand(i, 0)?;
            let src = operand(i, 1)?;
            // A = dst（マスク）, B = src（マスク）
            asm.push_operand(&dst, insn_addr, count);
            asm.body.push(W::LocalSet(A));
            asm.push_operand(&src, insn_addr, count);
            mask_i64(&mut asm.body, size);
            asm.body.push(W::LocalSet(B));
            // R = 計算（マスク）
            asm.body.push(W::LocalGet(A));
            asm.body.push(W::LocalGet(B));
            let is_sub = matches!(m, M::Sub | M::Cmp);
            match m {
                M::Add => asm.body.push(W::I64Add),
                M::Sub | M::Cmp => asm.body.push(W::I64Sub),
                M::And | M::Test => asm.body.push(W::I64And),
                M::Or => asm.body.push(W::I64Or),
                M::Xor => asm.body.push(W::I64Xor),
                _ => unreachable!(),
            }
            mask_i64(&mut asm.body, size);
            asm.body.push(W::LocalSet(R));
            // フラグ
            if matches!(m, M::And | M::Or | M::Xor | M::Test) {
                asm.flags_logic(size);
            } else {
                asm.flags_addsub(is_sub, size);
            }
            // 書き戻し（cmp と test はしない）
            if !matches!(m, M::Cmp | M::Test) {
                asm.body.push(W::LocalGet(R));
                emit_store_dst(asm, i, size, insn_addr, count)?;
            }
            Some(())
        }
        M::Inc | M::Dec => {
            let size = dst_size(i);
            let dst = operand(i, 0)?;
            asm.push_operand(&dst, insn_addr, count);
            asm.body.push(W::LocalSet(A));
            asm.body.push(W::I64Const(1));
            asm.body.push(W::LocalSet(B));
            asm.body.push(W::LocalGet(A));
            asm.body.push(W::LocalGet(B));
            asm.body.push(if m == M::Dec { W::I64Sub } else { W::I64Add });
            mask_i64(&mut asm.body, size);
            asm.body.push(W::LocalSet(R));
            // cf を退避（GA を一時に使う）→ フラグを書く → cf を戻す（inc/dec は cf を変えない）
            let cf = asm.off.cf;
            asm.body.push(W::LocalGet(CPU));
            asm.body.push(W::I32Load8U(MemArg { offset: cf, align: 0, memory_index: 0 }));
            asm.body.push(W::LocalSet(GA));
            asm.flags_addsub(m == M::Dec, size);
            asm.cpu();
            asm.body.push(W::LocalGet(GA));
            asm.body.push(W::I32Store8(MemArg { offset: cf, align: 0, memory_index: 0 }));
            asm.body.push(W::LocalGet(R));
            emit_store_dst(asm, i, size, insn_addr, count)
        }
        M::Not => {
            let size = dst_size(i);
            let dst = operand(i, 0)?;
            asm.push_operand(&dst, insn_addr, count);
            asm.body.push(W::I64Const(-1));
            asm.body.push(W::I64Xor);
            mask_i64(&mut asm.body, size);
            emit_store_dst(asm, i, size, insn_addr, count)
        }
        M::Neg => {
            let size = dst_size(i);
            let dst = operand(i, 0)?;
            asm.body.push(W::I64Const(0));
            asm.body.push(W::LocalSet(A));
            asm.push_operand(&dst, insn_addr, count);
            mask_i64(&mut asm.body, size);
            asm.body.push(W::LocalSet(B));
            asm.body.push(W::LocalGet(A));
            asm.body.push(W::LocalGet(B));
            asm.body.push(W::I64Sub);
            mask_i64(&mut asm.body, size);
            asm.body.push(W::LocalSet(R));
            asm.flags_addsub(true, size);
            asm.body.push(W::LocalGet(R));
            emit_store_dst(asm, i, size, insn_addr, count)
        }
        _ => None,
    }
}

fn src_size(i: &Instruction) -> usize {
    match i.op_kind(1) {
        OpKind::Register => i.op_register(1).size(),
        OpKind::Memory => i.memory_size().size(),
        _ => 4,
    }
}

/// ブロックを変換する。first の命令が対応しないなどで作れないときは None。
/// 戻り値は (wasm の関数本体の局所と命令列) ではなく、呼び出し側が module() に渡せる body
pub struct Compiled {
    pub locals: Vec<(u32, ValType)>,
    pub body: Vec<W<'static>>,
}

/// start から基本ブロックを変換する。pages_ptr はページ表の先頭（ホストの番地、i32）
pub fn compile_block(mem: &Memory, bits: u32, start: u64, max_block: usize) -> Option<Compiled> {
    let mut asm = Asm { body: Vec::new(), off: offs(), pages_ptr: mem.pages_ptr(), bits };
    let mut rip = start;
    let mut count: u32 = 0;
    loop {
        let bytes = mem.fetch(rip, 16).ok()?;
        let mut dec = Decoder::with_ip(bits, bytes, rip, DecoderOptions::NONE);
        let insn = dec.decode();
        if insn.is_invalid() {
            break;
        }
        let next = insn.next_ip();
        let flow = insn.flow_control();
        // 分岐で終わる
        if flow != FlowControl::Next {
            match insn.mnemonic() {
                M::Jmp if matches!(insn.op_kind(0), OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64) => {
                    asm.emit_exit(insn.near_branch_target(), count + 1);
                }
                _ if is_jcc(&insn) => {
                    let target = insn.near_branch_target();
                    // rip = cond ? target : next（Select）
                    asm.cpu();
                    asm.body.push(W::I64Const(target as i64));
                    asm.body.push(W::I64Const(next as i64));
                    asm.push_cond(insn.condition_code());
                    asm.body.push(W::Select);
                    asm.body.push(W::I64Store(MemArg { offset: asm.off.rip, align: 3, memory_index: 0 }));
                    asm.body.push(W::I32Const((count + 1) as i32));
                    asm.body.push(W::Return);
                }
                _ => {
                    // call/ret/その他の分岐はインタープリターに任せる（この命令の手前で終える）
                    if count == 0 {
                        return None;
                    }
                    asm.emit_exit(rip, count);
                }
            }
            return Some(finish(asm));
        }
        // 普通の命令
        if emit_insn(&mut asm, &insn, rip, count).is_none() {
            if count == 0 {
                return None;
            }
            asm.emit_exit(rip, count);
            return Some(finish(asm));
        }
        count += 1;
        rip = next;
        if count as usize >= max_block {
            asm.emit_exit(rip, count);
            return Some(finish(asm));
        }
    }
    if count == 0 {
        return None;
    }
    asm.emit_exit(rip, count);
    Some(finish(asm))
}

fn finish(asm: Asm) -> Compiled {
    Compiled { locals: vec![(3, ValType::I64), (2, ValType::I32)], body: asm.body }
}

fn is_jcc(i: &Instruction) -> bool {
    matches!(i.code(), c if (Code::Jo_rel8_16..=Code::Jg_rel32_64).contains(&c)) && i.condition_code() != ConditionCode::None && i.mnemonic() != M::Jmp
}
