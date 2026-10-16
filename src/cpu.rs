//! x86-64 の CPU の状態（汎用レジスタ、rip、フラグ、xmm）と、命令の引数（レジスタ、メモリ、即値）の読み書き
use crate::mem::{Fault, Memory, R};
use iced_x86::{CodeSize, ConditionCode, Instruction, OpKind, Register};

#[derive(Default, Clone, Copy, Debug)]
pub struct Flags {
    pub cf: bool,
    pub zf: bool,
    pub sf: bool,
    pub of: bool,
    pub pf: bool,
}

pub struct Cpu {
    /// rax、rcx、rdx、rbx、rsp、rbp、rsi、rdi、r8〜r15 の順（iced の Register と同じ並び）
    pub regs: [u64; 16],
    pub rip: u64,
    pub flags: Flags,
    pub xmm: [u128; 16],
    /// 実行した命令の数（ベンチと無限ループの見張り）
    pub steps: u64,
    /// push、pop、call、ret が動かす幅（バイト。32 ビットの DLL なら 4、64 ビットなら 8）
    pub width: u8,
    /// fs セグメントの基点（32 ビットの Windows ではスレッド環境ブロック TEB。fs:[0] が SEH の鎖の先頭）
    pub fs_base: u64,
    /// x87 の FPU（8 本のレジスタスタック）。本物は 80 ビットだが、音の処理には f64 で足りる。st(0) は fpu[fpu_top]
    pub fpu: [f64; 8],
    /// スタックの先頭（st(0)）の添字。push で 1 減り、pop で 1 増える（8 で回る）
    pub fpu_top: usize,
    /// x87 の C0〜C3 の比較の結果（fcom、fnstsw で使う。bit8=C0、bit14=C3 として状態ワードに入れる）
    pub fpu_c: (bool, bool, bool, bool),
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu { regs: [0; 16], rip: 0, flags: Flags::default(), xmm: [0; 16], steps: 0, width: 8, fs_base: 0, fpu: [0.0; 8], fpu_top: 0, fpu_c: (false, false, false, false) }
    }
}

pub const RSP: usize = 4;

/// size バイトの値の範囲（それより上を 0 にする）
pub fn mask(size: usize) -> u64 {
    if size >= 8 { u64::MAX } else { (1u64 << (size * 8)) - 1 }
}

/// size バイトの値の符号のビット
pub fn sign(v: u64, size: usize) -> bool {
    (v >> (size * 8 - 1)) & 1 == 1
}

/// size バイトの値を符号付きで 64 ビットに広げる
pub fn sext(v: u64, size: usize) -> u64 {
    if size >= 8 {
        v
    } else {
        let s = 64 - size * 8;
        (((v << s) as i64) >> s) as u64
    }
}

fn gpr(r: Register) -> R<(usize, usize, bool)> {
    let full = r.full_register();
    if !(Register::RAX..=Register::R15).contains(&full) {
        return Err(Fault(format!("register {r:?}")));
    }
    let high = matches!(r, Register::AH | Register::CH | Register::DH | Register::BH);
    Ok(((full as usize) - (Register::RAX as usize), r.size(), high))
}

pub fn xmm_index(r: Register) -> R<usize> {
    if (Register::XMM0..=Register::XMM15).contains(&r) {
        Ok(r as usize - Register::XMM0 as usize)
    } else {
        Err(Fault(format!("xmm {r:?}")))
    }
}

impl Cpu {
    pub fn get_reg(&self, r: Register) -> R<u64> {
        let (i, size, high) = gpr(r)?;
        Ok(if high { (self.regs[i] >> 8) & 0xff } else { self.regs[i] & mask(size) })
    }

    /// 32 ビットの書き込みは上位を 0 にする。8、16 ビットは上位を残す（x86-64 の決まり）
    pub fn set_reg(&mut self, r: Register, v: u64) -> R<()> {
        let (i, size, high) = gpr(r)?;
        let old = self.regs[i];
        self.regs[i] = match (size, high) {
            (_, true) => (old & !0xff00) | ((v & 0xff) << 8),
            (8, _) => v,
            (4, _) => v & 0xffff_ffff,
            (s, _) => (old & !mask(s)) | (v & mask(s)),
        };
        Ok(())
    }

    /// メモリの引数の番地（rip 相対は iced が絶対番地にしてある）
    pub fn addr(&self, i: &Instruction) -> R<u64> {
        let mut a = i.memory_displacement64();
        let base = i.memory_base();
        if base != Register::None && base != Register::RIP {
            a = a.wrapping_add(self.get_reg(base)?);
        }
        let index = i.memory_index();
        if index != Register::None {
            a = a.wrapping_add(self.get_reg(index)?.wrapping_mul(i.memory_index_scale() as u64));
        }
        // fs: が付いていれば TEB の基点を足す（32 ビットの Windows の SEH など）
        if i.segment_prefix() == Register::FS {
            a = a.wrapping_add(self.fs_base);
        }
        // 32 ビットのコードの実効アドレスは 32 ビットで回り込む（負のオフセットを足したとき 2^32 を超えないように）
        if i.code_size() == CodeSize::Code32 {
            a &= 0xffff_ffff;
        }
        Ok(a)
    }

    /// 汎用の引数を読む（レジスタ、メモリ、即値）。size は引数の大きさ（バイト）
    pub fn read_op(&self, mem: &Memory, i: &Instruction, n: u32) -> R<(u64, usize)> {
        match i.op_kind(n) {
            OpKind::Register => {
                let r = i.op_register(n);
                Ok((self.get_reg(r)?, r.size()))
            }
            OpKind::Memory => {
                let size = i.memory_size().size();
                Ok((mem.read_n(self.addr(i)?, size.min(8))?, size))
            }
            OpKind::Immediate8 | OpKind::Immediate16 | OpKind::Immediate32 | OpKind::Immediate64 | OpKind::Immediate8to16 | OpKind::Immediate8to32 | OpKind::Immediate8to64 | OpKind::Immediate32to64 => {
                Ok((i.immediate(n), 8))
            }
            k => Err(Fault(format!("operand {k:?} in {i}"))),
        }
    }

    pub fn write_op(&mut self, mem: &mut Memory, i: &Instruction, n: u32, v: u64) -> R<()> {
        match i.op_kind(n) {
            OpKind::Register => self.set_reg(i.op_register(n), v),
            OpKind::Memory => {
                let size = i.memory_size().size().min(8);
                mem.write_n(self.addr(i)?, size, v)
            }
            k => Err(Fault(format!("write operand {k:?} in {i}"))),
        }
    }

    /// xmm の引数を 128 ビットで読む（メモリなら memory_size だけ読み、残りは 0）
    pub fn read_xmm_op(&self, mem: &Memory, i: &Instruction, n: u32) -> R<u128> {
        match i.op_kind(n) {
            OpKind::Register => {
                let r = i.op_register(n);
                if let Ok(x) = xmm_index(r) { Ok(self.xmm[x]) } else { Ok(self.get_reg(r)? as u128) }
            }
            OpKind::Memory => {
                let size = i.memory_size().size();
                let a = self.addr(i)?;
                if size >= 16 { mem.read_u128(a) } else { Ok(mem.read_n(a, size)? as u128) }
            }
            k => Err(Fault(format!("xmm operand {k:?} in {i}"))),
        }
    }

    pub fn push(&mut self, mem: &mut Memory, v: u64) -> R<()> {
        let w = self.width as u64;
        self.regs[RSP] = self.regs[RSP].wrapping_sub(w);
        mem.write_n(self.regs[RSP], self.width as usize, v)
    }

    pub fn pop(&mut self, mem: &Memory) -> R<u64> {
        let v = mem.read_n(self.regs[RSP], self.width as usize)?;
        self.regs[RSP] = self.regs[RSP].wrapping_add(self.width as u64);
        Ok(v)
    }

    /// 結果からフラグ（zf、sf、pf）を決める。cf と of は呼ぶ側で
    pub fn set_szp(&mut self, v: u64, size: usize) {
        let v = v & mask(size);
        self.flags.zf = v == 0;
        self.flags.sf = sign(v, size);
        self.flags.pf = (v as u8).count_ones() % 2 == 0;
    }

    pub fn cond(&self, c: ConditionCode) -> bool {
        let f = self.flags;
        match c {
            ConditionCode::o => f.of,
            ConditionCode::no => !f.of,
            ConditionCode::b => f.cf,
            ConditionCode::ae => !f.cf,
            ConditionCode::e => f.zf,
            ConditionCode::ne => !f.zf,
            ConditionCode::be => f.cf || f.zf,
            ConditionCode::a => !f.cf && !f.zf,
            ConditionCode::s => f.sf,
            ConditionCode::ns => !f.sf,
            ConditionCode::p => f.pf,
            ConditionCode::np => !f.pf,
            ConditionCode::l => f.sf != f.of,
            ConditionCode::ge => f.sf == f.of,
            ConditionCode::le => f.zf || f.sf != f.of,
            ConditionCode::g => !f.zf && f.sf == f.of,
            ConditionCode::None => true,
        }
    }

    /// x87: st(0) を読む
    pub fn st(&self, n: usize) -> f64 {
        self.fpu[(self.fpu_top + n) % 8]
    }

    /// x87: st(n) に書く
    pub fn set_st(&mut self, n: usize, v: f64) {
        self.fpu[(self.fpu_top + n) % 8] = v;
    }

    /// x87: スタックに積む（top を 1 減らして st(0) にする）
    pub fn fpush(&mut self, v: f64) {
        self.fpu_top = (self.fpu_top + 7) % 8;
        self.fpu[self.fpu_top] = v;
    }

    /// x87: スタックから下ろす（st(0) を捨てて top を 1 進める）
    pub fn fpop(&mut self) -> f64 {
        let v = self.fpu[self.fpu_top];
        self.fpu_top = (self.fpu_top + 1) % 8;
        v
    }

    /// x87: 2 つの値を比べて C3、C2、C0 を決める（C2 は順序づけできないとき）
    pub fn fcom(&mut self, a: f64, b: f64) {
        self.fpu_c = if a.is_nan() || b.is_nan() {
            (true, false, true, true) // 比べられない: C3=C2=C0=1
        } else if a > b {
            (false, false, false, false)
        } else if a < b {
            (true, false, false, false) // C0=1
        } else {
            (false, false, false, true) // C3=1
        };
    }

    /// x87: 状態ワード（fnstsw）。C0 は bit8、C1 は bit9、C2 は bit10、C3 は bit14。top は bit11〜13
    pub fn fpu_status(&self) -> u16 {
        let (c0, c1, c2, c3) = self.fpu_c;
        (c0 as u16) << 8 | (c1 as u16) << 9 | (c2 as u16) << 10 | ((self.fpu_top as u16 & 7) << 11) | (c3 as u16) << 14
    }
}
