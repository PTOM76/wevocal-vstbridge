//! エミュレーターの本体。DLL の関数を Windows の 64 ビットの呼び出しの決まりで呼び、終わるまで命令を実行する。
//! プラグインからホストへの呼び出し（コールバック、import）は「入口の番地」に来たら Rust の関数に回す
use crate::cpu::{Cpu, RSP};
use crate::mem::{Fault, Memory, R};
use crate::{exec, sse};
use iced_x86::{Decoder, DecoderOptions, Instruction};
use std::collections::HashMap;

/// 関数から戻ってきた印の番地（呼ぶときに戻り先としてスタックに積む。ここに来たら実行を止める）
pub const RETURN_MAGIC: u64 = 0xFFFF_8000_FFFF_0000;
/// ホストの関数の入口の番地の始まり（1 つにつき 8 ずつ。メモリは割り当てない。飛び先がここなら Rust に回す） */
pub const THUNK_BASE: u64 = 0xFFFF_8000_0000_0000;
const STACK_TOP: u64 = 0x7FFF_0010_0000;
const STACK_SIZE: usize = 1 << 20;
/// 1 回の呼び出しで実行する命令の上限（無限ループの見張り）
const MAX_STEPS: u64 = 50_000_000;

/// ホストの関数。引数はレジスタとスタックから自分で読み、戻り値（rax に入れる）を返す
pub type HostFn = Box<dyn FnMut(&mut Emu) -> R<u64>>;

pub struct Emu {
    pub cpu: Cpu,
    pub mem: Memory,
    /// 入口の番地 → ホストの関数
    thunks: HashMap<u64, HostFn>,
    next_thunk: u64,
}

impl Emu {
    pub fn new() -> Self {
        let mut mem = Memory::new();
        // スタック（下に伸びるので、上の番地から STACK_SIZE 分）
        mem.map(STACK_TOP - STACK_SIZE as u64, STACK_SIZE);
        let mut cpu = Cpu::default();
        cpu.regs[RSP] = STACK_TOP - 0x1000;
        Emu { cpu, mem, thunks: HashMap::new(), next_thunk: THUNK_BASE }
    }

    /// ホストの関数に入口の番地を 1 つ割り当てる（その番地へ飛ぶと f が呼ばれる）
    pub fn add_thunk(&mut self, f: HostFn) -> u64 {
        let at = self.next_thunk;
        self.next_thunk += 8;
        self.thunks.insert(at, f);
        at
    }

    /// Windows x64 の呼び出しの決まりで func を呼び、rax（戻り値）を返す。
    /// 整数の引数は rcx、rdx、r8、r9 の順。浮動小数点は xmm0〜xmm3。5 つ目からはスタック。呼ぶ側が影の領域（32 バイト）を空ける
    pub fn call(&mut self, func: u64, args: &[Arg]) -> R<u64> {
        const IREG: [usize; 4] = [1, 2, 8, 9]; // rcx, rdx, r8, r9
        // 5 つ目以降の引数をスタックに積む（逆順）。影の領域の分も含めて 16 バイトにそろえる
        let stack_args: Vec<&Arg> = args.iter().skip(4).collect();
        let mut rsp = self.cpu.regs[RSP];
        // 影の領域 32 ＋ スタックの引数。呼び出し後に rsp が 16 の倍数になるよう積む
        let extra = 32 + stack_args.len() * 8;
        let total = if (extra + 8) % 16 == 0 { extra } else { extra + 8 };
        rsp -= total as u64;
        self.cpu.regs[RSP] = rsp;
        for (i, a) in stack_args.iter().enumerate() {
            self.mem.write_n(rsp + 32 + i as u64 * 8, 8, a.bits())?;
        }
        for (i, a) in args.iter().take(4).enumerate() {
            match a {
                Arg::Int(v) => self.cpu.regs[IREG[i]] = *v,
                Arg::F32(f) => self.cpu.xmm[i] = f.to_bits() as u128,
                Arg::F64(f) => self.cpu.xmm[i] = f.to_bits() as u128,
            }
        }
        // 戻り先の印を積んで実行
        self.cpu.regs[RSP] -= 8;
        self.mem.write_n(self.cpu.regs[RSP], 8, RETURN_MAGIC)?;
        self.cpu.rip = func;
        self.run()?;
        Ok(self.cpu.regs[0])
    }

    /// rip から命令を実行する。戻りの印に来たら止める。ホストの入口に飛んだらその関数を呼ぶ
    fn run(&mut self) -> R<()> {
        loop {
            let rip = self.cpu.rip;
            if rip == RETURN_MAGIC {
                return Ok(());
            }
            // ホストの関数の入口か（call でここへ飛ぶと、戻り先はスタックの先頭にある）
            if rip >= THUNK_BASE && rip < self.next_thunk {
                let ret = self.cpu.pop(&self.mem)?;
                let mut f = self.thunks.remove(&rip).ok_or_else(|| Fault(format!("thunk {rip:#x}")))?;
                let rax = f(self);
                self.thunks.insert(rip, f);
                self.cpu.regs[0] = rax?;
                self.cpu.rip = ret;
                continue;
            }
            self.step()?;
            self.cpu.steps += 1;
            if self.cpu.steps > MAX_STEPS {
                return Err(Fault("step limit".into()));
            }
        }
    }

    /// 命令を 1 つ読んで実行する。rip は命令の長さだけ進める（分岐の命令は中で rip を変える）
    fn step(&mut self) -> R<()> {
        let rip = self.cpu.rip;
        let bytes = self.mem.fetch(rip, 16)?;
        let mut dec = Decoder::with_ip(64, bytes, rip, DecoderOptions::NONE);
        let insn = dec.decode();
        if insn.is_invalid() {
            return Err(Fault(format!("decode {rip:#x}")));
        }
        // 既定では次の命令へ進める。分岐の命令は exec の中で rip を書き換える
        self.cpu.rip = insn.next_ip();
        self.run_insn(&insn)
    }

    fn run_insn(&mut self, insn: &Instruction) -> R<()> {
        if exec::exec(&mut self.cpu, &mut self.mem, insn)? {
            return Ok(());
        }
        if sse::exec(&mut self.cpu, &mut self.mem, insn)? {
            return Ok(());
        }
        Err(Fault(format!("unimplemented {insn} @ {:#x}", insn.ip())))
    }
}

/// 呼び出しの引数
pub enum Arg {
    Int(u64),
    F32(f32),
    F64(f64),
}

impl Arg {
    fn bits(&self) -> u64 {
        match self {
            Arg::Int(v) => *v,
            Arg::F32(f) => f.to_bits() as u64,
            Arg::F64(f) => f.to_bits(),
        }
    }
}
