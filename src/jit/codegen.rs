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
const F0: u32 = 6; // f32 作業用
const F1: u32 = 7;
const D0: u32 = 8; // f64 作業用
const D1: u32 = 9;
const FT: u32 = 10; // i32 ブロックの頭の x87 のスタックの先頭（fpu_top）
const T0: u32 = 11; // i32 作業用

mod x87;

fn off_reg(idx: usize) -> u64 {
    (offset_of!(Cpu, regs) + idx * 8) as u64
}
fn off_xmm(idx: usize) -> u64 {
    (offset_of!(Cpu, xmm) + idx * 16) as u64
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
    /// 今の命令で実際に書くフラグ（使われるもの）だけのビット。CF=1,ZF=2,SF=4,OF=8,PF=16
    flag_mask: u8,
    /// x87 を使うブロックか（FT を頭で読み、抜けるときに先頭と深さを書き戻す）
    fpu: bool,
    /// ブロックの頭からの x87 のスタックの先頭のずれ（積むと 1 減る。変換するときに数える）
    fdelta: i32,
}
const FCF: u8 = 1;
const FZF: u8 = 2;
const FSF: u8 = 4;
const FOF: u8 = 8;
const FPF: u8 = 16;
const FALL: u8 = 31;

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
    /// x87 のスタックの先頭と深さを、今のずれで書き戻す（ブロックから抜ける所で）
    fn fpu_sync(&mut self) {
        if !self.fpu || self.fdelta == 0 {
            return;
        }
        let (top, depth) = (offset_of!(Cpu, fpu_top) as u64, offset_of!(Cpu, fpu_depth) as u64);
        self.cpu();
        self.body.push(W::LocalGet(FT));
        self.body.push(W::I32Const(self.fdelta));
        self.body.push(W::I32Add);
        self.body.push(W::I32Const(7));
        self.body.push(W::I32And);
        self.body.push(W::I32Store(MemArg { offset: top, align: 2, memory_index: 0 }));
        // 積むと深さが 1 増える（先頭は 1 減る）
        self.cpu();
        self.cpu();
        self.body.push(W::I32Load(MemArg { offset: depth, align: 2, memory_index: 0 }));
        self.body.push(W::I32Const(-self.fdelta));
        self.body.push(W::I32Add);
        self.body.push(W::I32Store(MemArg { offset: depth, align: 2, memory_index: 0 }));
    }
    /// rip を v にして、count を積んで返す
    fn emit_exit(&mut self, v: u64, count: u32) {
        self.fpu_sync();
        self.cpu();
        self.body.push(W::I64Const(v as i64));
        self.body.push(W::I64Store(MemArg { offset: self.off.rip, align: 3, memory_index: 0 }));
        self.body.push(W::I32Const(count as i32));
        self.body.push(W::Return);
    }
    /// rip を命令の番地にし、bail=1 にして count を返す
    fn emit_bail(&mut self, insn_addr: u64, count: u32) {
        // 中断した命令はまだ何も変えていない前提（x87 は、出し入れをメモリの読み書きのあとにする）
        self.fpu_sync();
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
    /// zf を書く（R から）
    fn emit_zf(&mut self) {
        if self.flag_mask & FZF == 0 {
            return;
        }
        let zf = self.off.zf;
        self.cpu();
        self.body.push(W::LocalGet(R));
        self.body.push(W::I64Eqz);
        self.body.push(W::I32Store8(MemArg { offset: zf, align: 0, memory_index: 0 }));
    }
    fn emit_sf(&mut self, size: usize) {
        if self.flag_mask & FSF == 0 {
            return;
        }
        let sf = self.off.sf;
        self.cpu();
        sign_bit(&mut self.body, R, size);
        self.body.push(W::I32Store8(MemArg { offset: sf, align: 0, memory_index: 0 }));
    }
    fn emit_pf(&mut self) {
        if self.flag_mask & FPF == 0 {
            return;
        }
        let pf = self.off.pf;
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
    /// A, B（引数、マスク済み）と R（結果、マスク済み）から add/sub のフラグを書く
    fn flags_addsub(&mut self, sub: bool, size: usize) {
        self.emit_zf();
        self.emit_sf(size);
        self.emit_pf();
        if self.flag_mask & FCF != 0 {
            let cf = self.off.cf;
            self.cpu();
            if sub {
                self.body.push(W::LocalGet(A));
                self.body.push(W::LocalGet(B));
                self.body.push(W::I64LtU);
            } else if size == 8 {
                self.body.push(W::LocalGet(R));
                self.body.push(W::LocalGet(A));
                self.body.push(W::I64LtU);
            } else {
                self.body.push(W::LocalGet(A));
                self.body.push(W::LocalGet(B));
                self.body.push(W::I64Add);
                self.body.push(W::I64Const(((1u64 << (size * 8)) - 1) as i64));
                self.body.push(W::I64GtU);
            }
            self.body.push(W::I32Store8(MemArg { offset: cf, align: 0, memory_index: 0 }));
        }
        if self.flag_mask & FOF != 0 {
            let of = self.off.of;
            self.cpu();
            sign_bit(&mut self.body, A, size);
            sign_bit(&mut self.body, B, size);
            if sub {
                self.body.push(W::I32Ne);
            } else {
                self.body.push(W::I32Eq);
            }
            sign_bit(&mut self.body, R, size);
            sign_bit(&mut self.body, A, size);
            self.body.push(W::I32Ne);
            self.body.push(W::I32And);
            self.body.push(W::I32Store8(MemArg { offset: of, align: 0, memory_index: 0 }));
        }
    }
    /// R（マスク済み）から logic（and/or/xor/test）のフラグ（cf=0, of=0, zf, sf, pf）
    fn flags_logic(&mut self, size: usize) {
        if self.flag_mask & FCF != 0 {
            let cf = self.off.cf;
            self.cpu();
            self.body.push(W::I32Const(0));
            self.body.push(W::I32Store8(MemArg { offset: cf, align: 0, memory_index: 0 }));
        }
        if self.flag_mask & FOF != 0 {
            let of = self.off.of;
            self.cpu();
            self.body.push(W::I32Const(0));
            self.body.push(W::I32Store8(MemArg { offset: of, align: 0, memory_index: 0 }));
        }
        self.emit_zf();
        self.emit_sf(size);
        self.emit_pf();
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
            // inc/dec は cf を変えない。flags_addsub は mask に cf を含めない前提（生死解析で除く）
            asm.flags_addsub(m == M::Dec, size);
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
        M::Bt | M::Bts | M::Btr | M::Btc => {
            // 引数 0 がレジスタのときだけ（メモリはインタープリター）
            if i.op_kind(0) != OpKind::Register {
                return None;
            }
            let size = dst_size(i);
            let (idx, _) = gpr(i.op_register(0))?;
            // A = dst（マスク）
            asm.load_reg(idx);
            mask_i64(&mut asm.body, size);
            asm.body.push(W::LocalSet(A));
            // B = ビットの位置 = src % (size*8)
            let src = operand(i, 1)?;
            asm.push_operand(&src, insn_addr, count);
            asm.body.push(W::I64Const((size as i64) * 8));
            asm.body.push(W::I64RemU);
            asm.body.push(W::LocalSet(B));
            // R = 1 << B（ビット）
            asm.body.push(W::I64Const(1));
            asm.body.push(W::LocalGet(B));
            asm.body.push(W::I64Shl);
            asm.body.push(W::LocalSet(R));
            // cf = (A & bit) != 0
            asm.cpu();
            asm.body.push(W::LocalGet(A));
            asm.body.push(W::LocalGet(R));
            asm.body.push(W::I64And);
            asm.body.push(W::I64Const(0));
            asm.body.push(W::I64Ne);
            asm.body.push(W::I32Store8(MemArg { offset: asm.off.cf, align: 0, memory_index: 0 }));
            // bt 以外は dst を更新
            if m != M::Bt {
                asm.body.push(W::LocalGet(A));
                asm.body.push(W::LocalGet(R));
                match m {
                    M::Bts => asm.body.push(W::I64Or),
                    M::Btr => {
                        asm.body.push(W::I64Const(-1));
                        asm.body.push(W::I64Xor); // ~bit
                        asm.body.push(W::I64And);
                    }
                    _ => asm.body.push(W::I64Xor), // btc
                }
                mask_i64(&mut asm.body, size);
                asm.store_reg(idx, size);
            }
            Some(())
        }
        M::Shl | M::Shr | M::Sar => emit_shift(asm, i, insn_addr, count),
        M::Imul if i.op_count() >= 2 => emit_imul(asm, i, insn_addr, count),
        _ if is_setcc(m) => {
            asm.push_cond(i.condition_code());
            asm.body.push(W::I64ExtendI32U);
            emit_store_dst(asm, i, 1, insn_addr, count)
        }
        // 条件が偽なら書かない（上位 32 ビットも消さない。インタープリターと同じ）。読みは条件によらず行う
        _ if x87::is_x87(m) => x87::emit(asm, i, insn_addr, count),
        _ if is_cmov(m) => {
            let size = dst_size(i);
            let src = operand(i, 1)?;
            let (idx, _) = gpr(i.op_register(0))?;
            asm.push_operand(&src, insn_addr, count);
            asm.body.push(W::LocalSet(B));
            asm.push_cond(i.condition_code());
            asm.body.push(W::If(BlockType::Empty));
            asm.body.push(W::LocalGet(B));
            asm.store_reg(idx, size);
            asm.body.push(W::End);
            Some(())
        }
        _ => emit_sse(asm, i, insn_addr, count),
    }
}

/// i64 の符号を size バイトから広げる
fn sext_i64(b: &mut Vec<W<'static>>, size: usize) {
    match size {
        1 => b.push(W::I64Extend8S),
        2 => b.push(W::I64Extend16S),
        4 => b.push(W::I64Extend32S),
        _ => {}
    }
}

/// shl、shr、sar（即値か cl）。A = 値（マスク）、B = 数、R = 結果（マスク）。OF は変えない
fn emit_shift(asm: &mut Asm, i: &Instruction, insn_addr: u64, count: u32) -> Option<()> {
    let m = i.mnemonic();
    let size = dst_size(i);
    let imm = shift_count(i, size);
    if imm == Some(0) {
        return Some(());
    }
    asm.push_operand(&operand(i, 0)?, insn_addr, count);
    asm.body.push(W::LocalSet(A));
    match imm {
        Some(c) => asm.body.push(W::I64Const(c as i64)),
        None => {
            asm.push_operand(&operand(i, 1)?, insn_addr, count);
            asm.body.push(W::I64Const(if size == 8 { 63 } else { 31 }));
            asm.body.push(W::I64And);
        }
    }
    asm.body.push(W::LocalSet(B));
    // cl の数が 0 なら何もしない
    asm.body.push(W::Block(BlockType::Empty));
    if imm.is_none() {
        asm.body.push(W::LocalGet(B));
        asm.body.push(W::I64Eqz);
        asm.body.push(W::BrIf(0));
    }
    // 押し出す前の値（sar は符号を広げる）
    let src = |b: &mut Vec<W<'static>>| {
        b.push(W::LocalGet(A));
        if m == M::Sar {
            sext_i64(b, size);
        }
    };
    src(&mut asm.body);
    asm.body.push(W::LocalGet(B));
    asm.body.push(match m {
        M::Shl => W::I64Shl,
        M::Shr => W::I64ShrU,
        _ => W::I64ShrS,
    });
    mask_i64(&mut asm.body, size);
    asm.body.push(W::LocalSet(R));
    // cf = 最後に押し出されたビット
    if asm.flag_mask & FCF != 0 {
        let cf = asm.off.cf;
        asm.cpu();
        src(&mut asm.body);
        if m == M::Shl {
            asm.body.push(W::I64Const(size as i64 * 8));
            asm.body.push(W::LocalGet(B));
            asm.body.push(W::I64Sub);
            asm.body.push(W::I64ShrU);
        } else {
            asm.body.push(W::LocalGet(B));
            asm.body.push(W::I64Const(1));
            asm.body.push(W::I64Sub);
            asm.body.push(if m == M::Shr { W::I64ShrU } else { W::I64ShrS });
        }
        asm.body.push(W::I32WrapI64);
        asm.body.push(W::I32Const(1));
        asm.body.push(W::I32And);
        asm.body.push(W::I32Store8(MemArg { offset: cf, align: 0, memory_index: 0 }));
    }
    asm.emit_zf();
    asm.emit_sf(size);
    asm.emit_pf();
    asm.body.push(W::LocalGet(R));
    emit_store_dst(asm, i, size, insn_addr, count)?;
    asm.body.push(W::End);
    Some(())
}

/// imul の 2 つと 3 つの引数の形。cf と of は「結果が size に収まらない」（sf、zf、pf は変えない。インタープリターと同じ）
fn emit_imul(asm: &mut Asm, i: &Instruction, insn_addr: u64, count: u32) -> Option<()> {
    let size = dst_size(i);
    let (x, y) = if i.op_count() == 3 { (1, 2) } else { (0, 1) };
    asm.push_operand(&operand(i, x)?, insn_addr, count);
    sext_i64(&mut asm.body, size);
    asm.body.push(W::LocalSet(A));
    asm.push_operand(&operand(i, y)?, insn_addr, count);
    mask_i64(&mut asm.body, size);
    sext_i64(&mut asm.body, size);
    asm.body.push(W::LocalSet(B));
    asm.body.push(W::LocalGet(A));
    asm.body.push(W::LocalGet(B));
    asm.body.push(W::I64Mul);
    asm.body.push(W::LocalSet(R));
    if asm.flag_mask & (FCF | FOF) != 0 {
        // 収まらないか（i32 の 0/1 を GA に）
        if size < 8 {
            // 積は i64 に収まるので、size から符号を広げ直して比べる
            asm.body.push(W::LocalGet(R));
            sext_i64(&mut asm.body, size);
            asm.body.push(W::LocalGet(R));
            asm.body.push(W::I64Ne);
        } else {
            // a != 0 かつ（a == -1 なら b == MIN、そうでなければ r / a != b）。割り算の例外を避ける
            asm.body.push(W::LocalGet(A));
            asm.body.push(W::I64Eqz);
            asm.body.push(W::If(BlockType::Result(ValType::I32)));
            asm.body.push(W::I32Const(0));
            asm.body.push(W::Else);
            asm.body.push(W::LocalGet(A));
            asm.body.push(W::I64Const(-1));
            asm.body.push(W::I64Eq);
            asm.body.push(W::If(BlockType::Result(ValType::I32)));
            asm.body.push(W::LocalGet(B));
            asm.body.push(W::I64Const(i64::MIN));
            asm.body.push(W::I64Eq);
            asm.body.push(W::Else);
            asm.body.push(W::LocalGet(R));
            asm.body.push(W::LocalGet(A));
            asm.body.push(W::I64DivS);
            asm.body.push(W::LocalGet(B));
            asm.body.push(W::I64Ne);
            asm.body.push(W::End);
            asm.body.push(W::End);
        }
        asm.body.push(W::LocalSet(GA));
        for (bit, off) in [(FCF, asm.off.cf), (FOF, asm.off.of)] {
            if asm.flag_mask & bit != 0 {
                asm.cpu();
                asm.body.push(W::LocalGet(GA));
                asm.body.push(W::I32Store8(MemArg { offset: off, align: 0, memory_index: 0 }));
            }
        }
    }
    asm.body.push(W::LocalGet(R));
    mask_i64(&mut asm.body, size);
    emit_store_dst(asm, i, size, insn_addr, count)
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

/// ブロックの終わり方
enum Term {
    /// この番地に rip を置いて戻る（count は実行済みの数）
    ExitAt(u64),
    /// 無条件分岐
    Jmp(u64),
    /// 条件分岐（条件コード, 飛び先, 次）
    Jcc(ConditionCode, u64, u64),
}

/// 条件コードが読むフラグ
fn cond_reads(c: ConditionCode) -> u8 {
    use ConditionCode as C;
    match c {
        C::o | C::no => FOF,
        C::b | C::ae => FCF,
        C::e | C::ne => FZF,
        C::be | C::a => FCF | FZF,
        C::s | C::ns => FSF,
        C::p | C::np => FPF,
        C::l | C::ge => FSF | FOF,
        C::le | C::g => FZF | FSF | FOF,
        C::None => 0,
    }
}

fn is_cmov(m: M) -> bool {
    use M::*;
    matches!(m, Cmovo | Cmovno | Cmovb | Cmovae | Cmove | Cmovne | Cmovbe | Cmova | Cmovs | Cmovns | Cmovp | Cmovnp | Cmovl | Cmovge | Cmovle | Cmovg)
}
fn is_setcc(m: M) -> bool {
    use M::*;
    matches!(m, Seto | Setno | Setb | Setae | Sete | Setne | Setbe | Seta | Sets | Setns | Setp | Setnp | Setl | Setge | Setle | Setg)
}

/// 整数の引数がすべて JIT で扱える形か（ah などの上位バイト、rip 相対、fs/gs は扱わない）
fn int_operands_ok(i: &Instruction) -> bool {
    (0..i.op_count()).all(|n| operand(i, n).is_some())
}

/// シフトの数（即値なら Some(数)、cl なら None）。大きさで切り詰めた値
fn shift_count(i: &Instruction, size: usize) -> Option<u64> {
    let m = if size == 8 { 63 } else { 31 };
    match operand(i, 1)? {
        Opnd::Imm(v) => Some(v & m),
        _ => None,
    }
}

/// フラグの書き読み。kill は必ず書くもの（前の値を消す）、emit は書くことがあるもの、read は読むもの
struct Rw {
    kill: u8,
    emit: u8,
    read: u8,
}
const fn rw(kill: u8) -> Rw {
    Rw { kill, emit: kill, read: 0 }
}

/// その命令が JIT で変換できるか（できるならフラグの書き読み）。できないなら None
fn insn_rw(i: &Instruction) -> Option<Rw> {
    let m = i.mnemonic();
    if x87::is_x87(m) {
        return x87::rw(i);
    }
    if is_cmov(m) || is_setcc(m) {
        if !int_operands_ok(i) || (is_cmov(m) && i.op_kind(0) != OpKind::Register) {
            return None;
        }
        return Some(Rw { kill: 0, emit: 0, read: cond_reads(i.condition_code()) });
    }
    use M::*;
    Some(match m {
        // シフト（OF は変えない。インタープリターと同じ）。数が 0 なら何もしない
        Shl | Shr | Sar => {
            if !int_operands_ok(i) {
                return None;
            }
            let size = dst_size(i);
            let szpc = FCF | FZF | FSF | FPF;
            match shift_count(i, size) {
                Some(0) => rw(0),
                // 大きさを超える数（8、16 ビットだけ起きる）はインタープリター
                Some(c) if c >= size as u64 * 8 => return None,
                Some(_) => rw(szpc),
                // cl の数: 0 なら書かないので、前の値は消さない。8、16 ビットは大きさを超えうるのでインタープリター
                None if size >= 4 && i.op_register(1) == Register::CL => Rw { kill: 0, emit: szpc, read: 0 },
                None => return None,
            }
        }
        Imul if i.op_count() >= 2 => {
            if !int_operands_ok(i) {
                return None;
            }
            rw(FCF | FOF)
        }
        _ => rw(insn_flags(i)?),
    })
}

/// 書き込むフラグのビット（読みのない命令）。できないなら None
fn insn_flags(i: &Instruction) -> Option<u8> {
    use M::*;
    Some(match i.mnemonic() {
        Mov | Movzx | Movsx | Movsxd | Lea | Not | Nop | Endbr64 => 0,
        Add | Sub | Neg | Cmp => FALL,
        And | Or | Xor | Test => FALL,
        Inc | Dec => FZF | FSF | FOF | FPF,
        Bt | Bts | Btr | Btc => FCF,
        // SSE（フラグを書かない移動・算術・ビット演算）
        Movss | Movsd | Movaps | Movups | Movdqa | Movdqu | Movapd | Movupd => 0,
        Addss | Subss | Mulss | Divss | Minss | Maxss | Sqrtss => 0,
        Addsd | Subsd | Mulsd | Divsd | Minsd | Maxsd | Sqrtsd => 0,
        Addps | Subps | Mulps | Divps | Addpd | Subpd | Mulpd | Divpd => 0,
        Pxor | Xorps | Xorpd | Pand | Andps | Andpd | Por | Orps | Orpd | Pandn | Andnps | Andnpd => 0,
        Comiss | Ucomiss | Comisd | Ucomisd => FALL,
        _ => return None,
    })
}

/// start から基本ブロックを変換する。pages_ptr はページ表の先頭（ホストの番地、i32）
pub fn compile_block(mem: &Memory, bits: u32, start: u64, max_block: usize) -> Option<Compiled> {
    // 第 1 パス: 命令を集めて、終わり方を決める
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
    // フラグの生死（後ろ向き。ブロック末では全フラグ生存とみなす）。読まれる前に必ず上書きされるフラグは作らない
    let n = insns.len();
    let mut live = vec![0u8; n];
    let mut need = FALL;
    for k in (0..n).rev() {
        let f = insn_rw(&insns[k])?;
        live[k] = f.emit & need;
        need = (need & !f.kill) | f.read;
    }
    // 第 2 パス: 生成
    let fpu = insns.iter().any(|i| x87::is_x87(i.mnemonic()));
    let mut asm = Asm { body: Vec::new(), off: offs(), pages_ptr: mem.pages_ptr(), bits, flag_mask: FALL, fpu, fdelta: 0 };
    if fpu {
        asm.cpu();
        asm.body.push(W::I32Load(MemArg { offset: offset_of!(Cpu, fpu_top) as u64, align: 2, memory_index: 0 }));
        asm.body.push(W::LocalSet(FT));
    }
    for (k, insn) in insns.iter().enumerate() {
        asm.flag_mask = live[k];
        if emit_insn(&mut asm, insn, insn.ip(), k as u32).is_none() {
            // insn_flags が対応と判断したのに生成できない → 安全のためこのブロックは作らない
            return None;
        }
    }
    let count = n as u32;
    match term {
        Term::ExitAt(a) => asm.emit_exit(a, count),
        Term::Jmp(t) => asm.emit_exit(t, count),
        Term::Jcc(cc, t, next) => {
            asm.fpu_sync();
            asm.cpu();
            asm.body.push(W::I64Const(t as i64));
            asm.body.push(W::I64Const(next as i64));
            asm.push_cond(cc);
            asm.body.push(W::Select);
            asm.body.push(W::I64Store(MemArg { offset: asm.off.rip, align: 3, memory_index: 0 }));
            asm.body.push(W::I32Const(count as i32));
            asm.body.push(W::Return);
        }
    }
    Some(finish(asm))
}

fn finish(asm: Asm) -> Compiled {
    Compiled { locals: vec![(3, ValType::I64), (2, ValType::I32), (2, ValType::F32), (2, ValType::F64), (2, ValType::I32)], body: asm.body }
}

fn is_jcc(i: &Instruction) -> bool {
    matches!(i.code(), c if (Code::Jo_rel8_16..=Code::Jg_rel32_64).contains(&c)) && i.condition_code() != ConditionCode::None && i.mnemonic() != M::Jmp
}

/// 引数 op（xmm レジスタかメモリ）の先頭を指す i32 ホスト番地をスタックに積む。戻りは「16 で整列しているか」
fn xmm_ptr(asm: &mut Asm, i: &Instruction, n: u32, size: usize, insn_addr: u64, count: u32) -> Option<bool> {
    match i.op_kind(n) {
        OpKind::Register => {
            let idx = crate::cpu::xmm_index(i.op_register(n)).ok()?;
            asm.cpu();
            asm.body.push(W::I32Const(off_xmm(idx) as i32));
            asm.body.push(W::I32Add);
            Some(true)
        }
        OpKind::Memory => {
            let Opnd::Mem(mo, _) = operand(i, n)? else { return None };
            asm.calc_addr(&mo);
            asm.resolve_ptr(size, insn_addr, count);
            asm.body.push(W::LocalGet(HP));
            Some(false)
        }
        _ => None,
    }
}

fn f32ld(al: bool) -> W<'static> {
    W::F32Load(MemArg { offset: 0, align: if al { 2 } else { 0 }, memory_index: 0 })
}
fn f64ld(al: bool) -> W<'static> {
    W::F64Load(MemArg { offset: 0, align: if al { 3 } else { 0 }, memory_index: 0 })
}
fn v128ld(al: bool) -> W<'static> {
    W::V128Load(MemArg { offset: 0, align: if al { 4 } else { 0 }, memory_index: 0 })
}
fn xmm_dst(i: &Instruction) -> Option<usize> {
    if i.op_kind(0) != OpKind::Register {
        return None;
    }
    crate::cpu::xmm_index(i.op_register(0)).ok()
}

/// SSE の命令を変換する（対応しないなら None）
fn emit_sse(asm: &mut Asm, i: &Instruction, insn_addr: u64, count: u32) -> Option<()> {
    let m = i.mnemonic();
    match m {
        // スカラ 32 ビットの移動（movss）
        M::Movss => {
            match (i.op_kind(0), i.op_kind(1)) {
                (OpKind::Register, OpKind::Register) => {
                    // 上位を残して低 32 ビットを写す
                    let d = crate::cpu::xmm_index(i.op_register(0)).ok()?;
                    let s = crate::cpu::xmm_index(i.op_register(1)).ok()?;
                    asm.cpu();
                    asm.cpu();
                    asm.body.push(W::F32Load(MemArg { offset: off_xmm(s), align: 2, memory_index: 0 }));
                    asm.body.push(W::F32Store(MemArg { offset: off_xmm(d), align: 2, memory_index: 0 }));
                    Some(())
                }
                (OpKind::Register, OpKind::Memory) => {
                    // 上位を 0 にして低 32 ビットに読み込む
                    let d = crate::cpu::xmm_index(i.op_register(0)).ok()?;
                    asm.cpu();
                    asm.body.push(W::V128Const(0));
                    asm.body.push(W::V128Store(MemArg { offset: off_xmm(d), align: 4, memory_index: 0 }));
                    asm.cpu();
                    let al = xmm_ptr(asm, i, 1, 4, insn_addr, count)?;
                    asm.body.push(f32ld(al));
                    asm.body.push(W::F32Store(MemArg { offset: off_xmm(d), align: 2, memory_index: 0 }));
                    Some(())
                }
                (OpKind::Memory, OpKind::Register) => {
                    let s = crate::cpu::xmm_index(i.op_register(1)).ok()?;
                    let al = xmm_ptr(asm, i, 0, 4, insn_addr, count)?;
                    asm.cpu();
                    asm.body.push(W::F32Load(MemArg { offset: off_xmm(s), align: 2, memory_index: 0 }));
                    asm.body.push(W::F32Store(MemArg { offset: 0, align: if al { 2 } else { 0 }, memory_index: 0 }));
                    Some(())
                }
                _ => None,
            }
        }
        // スカラ 64 ビットの移動（movsd）
        M::Movsd => {
            match (i.op_kind(0), i.op_kind(1)) {
                (OpKind::Register, OpKind::Register) => {
                    let d = crate::cpu::xmm_index(i.op_register(0)).ok()?;
                    let s = crate::cpu::xmm_index(i.op_register(1)).ok()?;
                    asm.cpu();
                    asm.cpu();
                    asm.body.push(W::F64Load(MemArg { offset: off_xmm(s), align: 3, memory_index: 0 }));
                    asm.body.push(W::F64Store(MemArg { offset: off_xmm(d), align: 3, memory_index: 0 }));
                    Some(())
                }
                (OpKind::Register, OpKind::Memory) => {
                    let d = crate::cpu::xmm_index(i.op_register(0)).ok()?;
                    asm.cpu();
                    asm.body.push(W::V128Const(0));
                    asm.body.push(W::V128Store(MemArg { offset: off_xmm(d), align: 4, memory_index: 0 }));
                    asm.cpu();
                    let al = xmm_ptr(asm, i, 1, 8, insn_addr, count)?;
                    asm.body.push(f64ld(al));
                    asm.body.push(W::F64Store(MemArg { offset: off_xmm(d), align: 3, memory_index: 0 }));
                    Some(())
                }
                (OpKind::Memory, OpKind::Register) => {
                    let s = crate::cpu::xmm_index(i.op_register(1)).ok()?;
                    let al = xmm_ptr(asm, i, 0, 8, insn_addr, count)?;
                    asm.cpu();
                    asm.body.push(W::F64Load(MemArg { offset: off_xmm(s), align: 3, memory_index: 0 }));
                    asm.body.push(W::F64Store(MemArg { offset: 0, align: if al { 3 } else { 0 }, memory_index: 0 }));
                    Some(())
                }
                _ => None,
            }
        }
        // 128 ビットの移動
        M::Movaps | M::Movups | M::Movdqa | M::Movdqu | M::Movapd | M::Movupd => {
            if let Some(d) = xmm_dst(i) {
                asm.cpu(); // store 先
                let al = xmm_ptr(asm, i, 1, 16, insn_addr, count)?;
                asm.body.push(v128ld(al));
                asm.body.push(W::V128Store(MemArg { offset: off_xmm(d), align: 4, memory_index: 0 }));
                Some(())
            } else if i.op_kind(1) == OpKind::Register {
                // メモリ ← xmm
                let s = crate::cpu::xmm_index(i.op_register(1)).ok()?;
                let al = xmm_ptr(asm, i, 0, 16, insn_addr, count)?; // dst のメモリ番地
                asm.cpu();
                asm.body.push(W::V128Load(MemArg { offset: off_xmm(s), align: 4, memory_index: 0 }));
                asm.body.push(W::V128Store(MemArg { offset: 0, align: if al { 4 } else { 0 }, memory_index: 0 }));
                Some(())
            } else {
                None
            }
        }
        // スカラ 32 ビット float の二項演算
        M::Addss | M::Subss | M::Mulss | M::Divss | M::Minss | M::Maxss | M::Sqrtss => {
            let d = xmm_dst(i)?;
            asm.cpu();
            asm.body.push(W::F32Load(MemArg { offset: off_xmm(d), align: 2, memory_index: 0 }));
            asm.body.push(W::LocalSet(F0));
            let al = xmm_ptr(asm, i, 1, 4, insn_addr, count)?;
            asm.body.push(f32ld(al));
            asm.body.push(W::LocalSet(F1));
            asm.cpu();
            match m {
                M::Sqrtss => {
                    asm.body.push(W::LocalGet(F1));
                    asm.body.push(W::F32Sqrt);
                }
                M::Minss | M::Maxss => {
                    asm.body.push(W::LocalGet(F0));
                    asm.body.push(W::LocalGet(F1));
                    asm.body.push(W::LocalGet(F0));
                    asm.body.push(W::LocalGet(F1));
                    asm.body.push(if m == M::Minss { W::F32Lt } else { W::F32Gt });
                    asm.body.push(W::Select);
                }
                _ => {
                    asm.body.push(W::LocalGet(F0));
                    asm.body.push(W::LocalGet(F1));
                    asm.body.push(match m {
                        M::Addss => W::F32Add,
                        M::Subss => W::F32Sub,
                        M::Mulss => W::F32Mul,
                        _ => W::F32Div,
                    });
                }
            }
            asm.body.push(W::F32Store(MemArg { offset: off_xmm(d), align: 2, memory_index: 0 }));
            Some(())
        }
        // スカラ 64 ビット float
        M::Addsd | M::Subsd | M::Mulsd | M::Divsd | M::Minsd | M::Maxsd | M::Sqrtsd => {
            let d = xmm_dst(i)?;
            asm.cpu();
            asm.body.push(W::F64Load(MemArg { offset: off_xmm(d), align: 3, memory_index: 0 }));
            asm.body.push(W::LocalSet(D0));
            let al = xmm_ptr(asm, i, 1, 8, insn_addr, count)?;
            asm.body.push(f64ld(al));
            asm.body.push(W::LocalSet(D1));
            asm.cpu();
            match m {
                M::Sqrtsd => {
                    asm.body.push(W::LocalGet(D1));
                    asm.body.push(W::F64Sqrt);
                }
                M::Minsd | M::Maxsd => {
                    asm.body.push(W::LocalGet(D0));
                    asm.body.push(W::LocalGet(D1));
                    asm.body.push(W::LocalGet(D0));
                    asm.body.push(W::LocalGet(D1));
                    asm.body.push(if m == M::Minsd { W::F64Lt } else { W::F64Gt });
                    asm.body.push(W::Select);
                }
                _ => {
                    asm.body.push(W::LocalGet(D0));
                    asm.body.push(W::LocalGet(D1));
                    asm.body.push(match m {
                        M::Addsd => W::F64Add,
                        M::Subsd => W::F64Sub,
                        M::Mulsd => W::F64Mul,
                        _ => W::F64Div,
                    });
                }
            }
            asm.body.push(W::F64Store(MemArg { offset: off_xmm(d), align: 3, memory_index: 0 }));
            Some(())
        }
        // packed 32 ビット float
        M::Addps | M::Subps | M::Mulps | M::Divps => {
            let d = xmm_dst(i)?;
            asm.cpu(); // store 先
            asm.cpu();
            asm.body.push(W::V128Load(MemArg { offset: off_xmm(d), align: 4, memory_index: 0 }));
            let al = xmm_ptr(asm, i, 1, 16, insn_addr, count)?;
            asm.body.push(v128ld(al));
            asm.body.push(match m {
                M::Addps => W::F32x4Add,
                M::Subps => W::F32x4Sub,
                M::Mulps => W::F32x4Mul,
                _ => W::F32x4Div,
            });
            asm.body.push(W::V128Store(MemArg { offset: off_xmm(d), align: 4, memory_index: 0 }));
            Some(())
        }
        // packed 64 ビット float
        M::Addpd | M::Subpd | M::Mulpd | M::Divpd => {
            let d = xmm_dst(i)?;
            asm.cpu();
            asm.cpu();
            asm.body.push(W::V128Load(MemArg { offset: off_xmm(d), align: 4, memory_index: 0 }));
            let al = xmm_ptr(asm, i, 1, 16, insn_addr, count)?;
            asm.body.push(v128ld(al));
            asm.body.push(match m {
                M::Addpd => W::F64x2Add,
                M::Subpd => W::F64x2Sub,
                M::Mulpd => W::F64x2Mul,
                _ => W::F64x2Div,
            });
            asm.body.push(W::V128Store(MemArg { offset: off_xmm(d), align: 4, memory_index: 0 }));
            Some(())
        }
        // 128 ビットのビット演算
        M::Pxor | M::Xorps | M::Xorpd | M::Pand | M::Andps | M::Andpd | M::Por | M::Orps | M::Orpd | M::Pandn | M::Andnps | M::Andnpd => {
            let d = xmm_dst(i)?;
            asm.cpu(); // store 先
            if matches!(m, M::Pandn | M::Andnps | M::Andnpd) {
                // ~dst & src = AndNot(src, dst)
                let al = xmm_ptr(asm, i, 1, 16, insn_addr, count)?;
                asm.body.push(v128ld(al));
                asm.cpu();
                asm.body.push(W::V128Load(MemArg { offset: off_xmm(d), align: 4, memory_index: 0 }));
                asm.body.push(W::V128AndNot);
            } else {
                asm.cpu();
                asm.body.push(W::V128Load(MemArg { offset: off_xmm(d), align: 4, memory_index: 0 }));
                let al = xmm_ptr(asm, i, 1, 16, insn_addr, count)?;
                asm.body.push(v128ld(al));
                asm.body.push(match m {
                    M::Pxor | M::Xorps | M::Xorpd => W::V128Xor,
                    M::Pand | M::Andps | M::Andpd => W::V128And,
                    _ => W::V128Or,
                });
            }
            asm.body.push(W::V128Store(MemArg { offset: off_xmm(d), align: 4, memory_index: 0 }));
            Some(())
        }
        // 比較（フラグ）。zf=un||a==b, pf=un, cf=un||a<b, sf=of=0
        M::Comiss | M::Ucomiss | M::Comisd | M::Ucomisd => {
            let d = crate::cpu::xmm_index(i.op_register(0)).ok()?;
            let ss = matches!(m, M::Comiss | M::Ucomiss);
            if ss {
                asm.cpu();
                asm.body.push(W::F32Load(MemArg { offset: off_xmm(d), align: 2, memory_index: 0 }));
                asm.body.push(W::F64PromoteF32);
                asm.body.push(W::LocalSet(D0));
                let al = xmm_ptr(asm, i, 1, 4, insn_addr, count)?;
                asm.body.push(f32ld(al));
                asm.body.push(W::F64PromoteF32);
                asm.body.push(W::LocalSet(D1));
            } else {
                asm.cpu();
                asm.body.push(W::F64Load(MemArg { offset: off_xmm(d), align: 3, memory_index: 0 }));
                asm.body.push(W::LocalSet(D0));
                let al = xmm_ptr(asm, i, 1, 8, insn_addr, count)?;
                asm.body.push(f64ld(al));
                asm.body.push(W::LocalSet(D1));
            }
            let (cf, zf, sf, of, pf) = (asm.off.cf, asm.off.zf, asm.off.sf, asm.off.of, asm.off.pf);
            // un = (a!=a)|(b!=b)
            // zf = un | (a==b)
            asm.cpu();
            asm.body.push(W::LocalGet(D0));
            asm.body.push(W::LocalGet(D0));
            asm.body.push(W::F64Ne);
            asm.body.push(W::LocalGet(D1));
            asm.body.push(W::LocalGet(D1));
            asm.body.push(W::F64Ne);
            asm.body.push(W::I32Or); // un
            asm.body.push(W::LocalGet(D0));
            asm.body.push(W::LocalGet(D1));
            asm.body.push(W::F64Eq);
            asm.body.push(W::I32Or);
            asm.body.push(W::I32Store8(MemArg { offset: zf, align: 0, memory_index: 0 }));
            // pf = un
            asm.cpu();
            asm.body.push(W::LocalGet(D0));
            asm.body.push(W::LocalGet(D0));
            asm.body.push(W::F64Ne);
            asm.body.push(W::LocalGet(D1));
            asm.body.push(W::LocalGet(D1));
            asm.body.push(W::F64Ne);
            asm.body.push(W::I32Or);
            asm.body.push(W::I32Store8(MemArg { offset: pf, align: 0, memory_index: 0 }));
            // cf = un | (a<b)
            asm.cpu();
            asm.body.push(W::LocalGet(D0));
            asm.body.push(W::LocalGet(D0));
            asm.body.push(W::F64Ne);
            asm.body.push(W::LocalGet(D1));
            asm.body.push(W::LocalGet(D1));
            asm.body.push(W::F64Ne);
            asm.body.push(W::I32Or);
            asm.body.push(W::LocalGet(D0));
            asm.body.push(W::LocalGet(D1));
            asm.body.push(W::F64Lt);
            asm.body.push(W::I32Or);
            asm.body.push(W::I32Store8(MemArg { offset: cf, align: 0, memory_index: 0 }));
            // sf=0, of=0
            asm.cpu();
            asm.body.push(W::I32Const(0));
            asm.body.push(W::I32Store8(MemArg { offset: sf, align: 0, memory_index: 0 }));
            asm.cpu();
            asm.body.push(W::I32Const(0));
            asm.body.push(W::I32Store8(MemArg { offset: of, align: 0, memory_index: 0 }));
            Some(())
        }
        _ => None,
    }
}
