//! エミュレーターの本体。DLL の関数を呼び、終わるまで命令を実行する（32 ビットと 64 ビットの両方に対応）。
//! プラグインからホストへの呼び出し（コールバック、import）は「入口の番地」に来たら Rust の関数に回す
use crate::cpu::{Cpu, RSP};
use crate::mem::{Fault, Memory, R};
use crate::fast::{self, Op};
use crate::{exec, sse};
use iced_x86::{Decoder, DecoderOptions, Instruction};
use std::collections::HashMap;

/// 関数から戻ってきた印の番地（32 ビットでも 4 バイトの番地に収まる範囲。呼ぶときに戻り先としてスタックに積む）
pub const RETURN_MAGIC: u64 = 0x4FFF_FFF0;
/// ホストの関数の入口の番地の始まり（32 ビットの DLL でも IAT（4 バイト）に書けるよう、2^32 未満にする）
pub const THUNK_BASE: u64 = 0x5000_0000;
/// 64 ビットでも 4GB 未満に置く（ページ表の近道と JIT のため）
const STACK_TOP: u64 = 0x7FFE_0000;
/// 32 ビットの DLL 用のスタック（2^32 未満の番地。ESP が 32 ビットなので、ここでないと番地が入らない）
const STACK_TOP_32: u64 = 0x7FFE_0000;
const STACK_SIZE: usize = 1 << 20;
/// 1 回の呼び出しで実行する命令の上限（無限ループの見張り）
/// 番地を引くだけの速いハッシュ（標準の SipHash は命令 1 つあたりには重い）
#[derive(Default, Clone, Copy)]
pub struct FastHasher(u64);
impl std::hash::Hasher for FastHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0.rotate_left(5) ^ *b as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = (v ^ (v >> 15)).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}
pub type FastHash = std::hash::BuildHasherDefault<FastHasher>;

/// call32 などが「途中で中断した」ときのエラーの文（Fault の中身）。画面の入力待ちのループで使う（gui.rs）
pub const YIELD: &str = "yield";
pub const MAX_STEPS: u64 = 50_000_000;
/// 読み込み（初期化）のときの上限。大きなプラグインは表の計算（FFT など）に数十億命令かかる
pub const LOAD_MAX_STEPS: u64 = 20_000_000_000;

/// ホストの関数。引数はレジスタとスタックから自分で読み、戻り値（rax に入れる）を返す
pub type HostFn = Box<dyn Fn(&mut Emu) -> R<u64>>;

pub struct Emu {
    pub cpu: Cpu,
    pub mem: Memory,
    /// 命令を読む幅（32 または 64）
    pub(crate) bits: u32,
    /// 入口の番地 → ホストの関数と、stdcall のとき呼ばれた側が片付ける引数のバイト数（cdecl なら 0）
    thunks: HashMap<u64, (std::rc::Rc<dyn Fn(&mut Emu) -> R<u64>>, u64)>,
    next_thunk: u64,
    /// LoadLibraryA で「読み込んだ」ことにした DLL の名前（ハンドルは添字に基づく）。GetProcAddress の解決に使う
    pub loaded: Vec<String>,
    /// GetProcAddress で作った入口の番地（DLL 名と関数名ごとに 1 つだけ作る）
    pub procs: HashMap<(String, String), u64>,
    /// ホストのヒープで確保したブロックの大きさ（番地 → バイト数）。HeapReAlloc で中身を写すために覚える
    pub heap_sizes: HashMap<u64, usize>,
    /// 読み込んだ DLL の番地（GetModuleHandle(NULL) などで返す。0 なら 32 ビットの既定）
    pub image_base: u64,
    /// 1 回の呼び出しの命令数の上限（無限ループで止まらないように）
    pub max_steps: u64,
    /// x87 で最初に NaN になる命令を見張る（環境変数 VSTBRIDGE_NAN があるときだけ）
    nan_watch: bool,
    /// この呼び出しが始まったときの命令数（無限ループの見張りは、呼び出しごとに数える）
    pub(crate) call_start: u64,
    /// ホストの関数が「ここで中断してほしい」と要求した（run が YIELD で止まる。再開は resume）
    pub yield_req: bool,
    /// 全命令とレジスタを出す（環境変数 VSTBRIDGE_TRACE。毎命令は読まないよう、作るときに 1 度だけ読む）
    trace: bool,
    /// デコード済みの命令（番地 → 命令）。コードが書き換わる（自己展開が終わったあと）ことはないとみなす。展開が終わってから使うため、書き込みで無効にする
    decoded: HashMap<u64, (Instruction, Op), FastHash>,
    /// 前もって解いた形（fast.rs）を使うか（トレースや調査のときは使わない）
    use_fast: bool,
    pub(crate) blocks: crate::blocks::Blocks,
    pub jit: crate::jit::Jit,
    pub jit_insns: u64,
    /// JIT が有効なのにインタープリターで動かしたブロックの先頭の命令（名前 → 回数。調査用）
    pub jit_miss: HashMap<String, u64>,
    pub interp_insns: u64,
    /// エディター（画面）の状態。ウィンドウ、GDI のオブジェクト、入力（gui.rs）
    pub gui: crate::gui::Gui,
    /// 命令ごとの回数と時間（VSTBRIDGE_PROF=1。調査用）
    pub prof: Option<HashMap<u32, (u64, u64)>>,
}

impl Emu {
    /// 64 ビットの DLL（x64）を動かすための空の状態
    pub fn new() -> Self {
        Self::with_bits(64)
    }

    /// 32 ビットの DLL（x86）を動かすための空の状態
    pub fn new32() -> Self {
        Self::with_bits(32)
    }

    fn with_bits(bits: u32) -> Self {
        let width = if bits == 32 { 4 } else { 8 };
        let top = if bits == 32 { STACK_TOP_32 } else { STACK_TOP };
        let mut mem = Memory::new();
        mem.map(top - STACK_SIZE as u64, STACK_SIZE);
        let mut cpu = Cpu { width, ..Default::default() };
        cpu.regs[RSP] = top - 0x1000;
        if bits == 32 {
            // スレッド環境ブロック（TEB）の最小限。fs:[0]=SEH の鎖の先頭（0xFFFFFFFF は鎖の終わり）、fs:[4]=スタックの上限、fs:[8]=下限
            let teb = 0x7FFD_0000u64;
            mem.map(teb, 0x1000);
            let _ = mem.write_n(teb, 4, 0xffff_ffff);
            let _ = mem.write_n(teb + 4, 4, top);
            let _ = mem.write_n(teb + 8, 4, top - STACK_SIZE as u64);
            let _ = mem.write_n(teb + 0x18, 4, teb); // 自分自身を指す（fs:[0x18]）
            cpu.fs_base = teb;
        } else {
            // 64 ビットの TEB の最小限（gs: で読む）。gs:[8]=スタックの上限、gs:[0x10]=下限（__chkstk が見る）、gs:[0x30]=自分自身、gs:[0x60]=PEB（中身は 0）
            let teb = 0x7FFD_0000u64;
            mem.map(teb, 0x2000);
            let _ = mem.write_n(teb + 8, 8, top);
            let _ = mem.write_n(teb + 0x10, 8, top - STACK_SIZE as u64);
            let _ = mem.write_n(teb + 0x30, 8, teb);
            let _ = mem.write_n(teb + 0x60, 8, teb + 0x1000);
            cpu.fs_base = teb;
        }
        Emu { cpu, mem, bits, thunks: HashMap::new(), next_thunk: THUNK_BASE, loaded: Vec::new(), procs: HashMap::new(), heap_sizes: HashMap::new(), image_base: 0, max_steps: MAX_STEPS, nan_watch: std::env::var("VSTBRIDGE_NAN").is_ok(), call_start: 0, yield_req: false, trace: std::env::var("VSTBRIDGE_TRACE").is_ok(), decoded: HashMap::default(), blocks: Default::default(), jit: Default::default(), jit_insns: 0, jit_miss: HashMap::new(), interp_insns: 0, gui: crate::gui::Gui::new(), use_fast: std::env::var("VSTBRIDGE_NOFAST").is_err() && std::env::var("VSTBRIDGE_TRACE").is_err() && std::env::var("VSTBRIDGE_NAN").is_err(), prof: if std::env::var("VSTBRIDGE_PROF").is_ok() { Some(HashMap::new()) } else { None } }
    }

    /// ホストの関数に入口の番地を 1 つ割り当てる（その番地へ飛ぶと f が呼ばれる）
    pub fn add_thunk(&mut self, f: HostFn) -> u64 {
        self.add_thunk_stdcall(f, 0)
    }

    /// stdcall のホスト関数。呼ばれた側（ここ）が引数を args 個ぶん片付ける（Windows の API はほとんどこれ）
    pub fn add_thunk_stdcall(&mut self, f: HostFn, args: u64) -> u64 {
        let at = self.next_thunk;
        self.next_thunk += self.cpu.width as u64;
        self.thunks.insert(at, (std::rc::Rc::from(f), args * self.cpu.width as u64));
        at
    }

    /// Windows x64 の呼び出しの決まりで func を呼び、rax（戻り値）を返す。
    /// 整数の引数は rcx、rdx、r8、r9 の順。浮動小数点は xmm0〜xmm3。5 つ目からはスタック。呼ぶ側が影の領域（32 バイト）を空ける
    pub fn call(&mut self, func: u64, args: &[Arg]) -> R<u64> {
        const IREG: [usize; 4] = [1, 2, 8, 9]; // rcx, rdx, r8, r9
        let stack_args: Vec<&Arg> = args.iter().skip(4).collect();
        let mut rsp = self.cpu.regs[RSP];
        // ホストの関数の中から呼ばれる（ウィンドウプロシージャなど）ことがあるので、戻ったあとに rsp と命令数の見張りの起点を戻す
        let (saved_rsp, saved_start) = (rsp, self.call_start);
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
        self.cpu.push(&mut self.mem, RETURN_MAGIC)?;
        self.cpu.rip = func;
        let r = self.run();
        self.call_start = saved_start;
        r?;
        self.cpu.regs[RSP] = saved_rsp;
        Ok(self.cpu.regs[0])
    }

    /// Windows x86（32 ビット）の呼び出しの決まり（cdecl）で func を呼ぶ。引数は右から左の順でスタックに積む。
    /// 戻ったあとのスタックの後始末は、呼び出した側がすることになっているが、ここでは戻り値（eax）だけ読んで捨てるので不要
    pub fn call32(&mut self, func: u32, args: &[Arg]) -> R<u64> {
        for a in args.iter().rev() {
            self.cpu.push(&mut self.mem, a.bits())?;
        }
        self.cpu.push(&mut self.mem, RETURN_MAGIC)?;
        self.cpu.rip = func as u64;
        // ホストの関数の中から呼ばれる（ウィンドウプロシージャなど）ことがあるので、命令数の見張りの起点を戻す
        let saved = self.call_start;
        let r = self.run();
        self.call_start = saved;
        r?;
        Ok(self.cpu.regs[0] & 0xffff_ffff)
    }

    /// rip がホストの関数の入口なら、それを呼んで戻り先へ進める（true）。そうでなければ false
    pub(crate) fn try_thunk(&mut self, rip: u64) -> R<bool> {
        if rip >= THUNK_BASE && rip < self.next_thunk && self.thunks.contains_key(&rip) {
            let ret = self.cpu.pop(&self.mem)?;
            // 入口は消さずに呼ぶ（ホストの関数の中から、また同じ関数が呼ばれることがある。ウィンドウの作成の中で子ウィンドウを作るなど）
            let (f, pop) = self.thunks.get(&rip).map(|(f, p)| (f.clone(), *p)).ok_or_else(|| Fault(format!("thunk {rip:#x}")))?;
            let rax = f(self);
            self.cpu.regs[0] = rax?;
            // stdcall は、呼ばれた側が引数を片付ける（ret N と同じ）
            self.cpu.regs[RSP] = self.cpu.regs[RSP].wrapping_add(pop);
            self.cpu.rip = ret;
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn next_thunk(&self) -> u64 {
        self.next_thunk
    }

    pub(crate) fn bits(&self) -> u32 {
        self.bits
    }

    pub(crate) fn decoded_clear(&mut self) {
        self.decoded.clear();
    }

    /// 中断した呼び出し（YIELD）の続きを動かす。戻りの印に来たら Ok、また中断したら Err(YIELD)
    pub fn resume(&mut self) -> R<()> {
        self.run()
    }

    /// rip から命令を実行する。戻りの印に来たら止める。ホストの入口に飛んだらその関数を呼ぶ
    fn run(&mut self) -> R<()> {
        if self.use_fast {
            return self.run_blocks();
        }
        self.call_start = self.cpu.steps;
        loop {
            let rip = self.cpu.rip;
            if rip == RETURN_MAGIC {
                return Ok(());
            }
            if self.try_thunk(rip)? {
                if self.yield_req {
                    self.yield_req = false;
                    return Err(Fault(YIELD.into()));
                }
                continue;
            }
            self.step()?;
            self.cpu.steps += 1;
            // 1 回の呼び出しの中での命令数の上限（累積ではなく、この呼び出しが始まってからの数）
            if self.cpu.steps - self.call_start > self.max_steps {
                return Err(Fault(format!("step limit (rip={:#x})", self.cpu.rip)));
            }
        }
    }

    /// 命令を 1 つ読んで実行する。rip は命令の長さだけ進める（分岐の命令は中で rip を変える）
    pub(crate) fn step(&mut self) -> R<()> {
        let rip = self.cpu.rip;
        // 読み取った命令を番地ごとに覚えておく（同じ処理を何度も回すので、デコードは 1 回で済む）
        // コードの領域に書き込みがあったら（自己展開、コードの書き換え）、覚えた命令を全部捨てる
        if self.mem.code_dirty {
            self.mem.code_dirty = false;
            self.decoded.clear();
        }
        let (insn, op) = match self.decoded.get(&rip) {
            Some((i, op)) => (*i, *op),
            None => {
                let bytes = self.mem.fetch(rip, 16)?;
                let mut dec = Decoder::with_ip(self.bits, bytes, rip, DecoderOptions::NONE);
                let insn = dec.decode();
                if insn.is_invalid() {
                    return Err(Fault(format!("decode {rip:#x}")));
                }
                let op = if self.use_fast { fast::compile(&insn) } else { Op::Generic };
                self.decoded.insert(rip, (insn, op));
                (insn, op)
            }
        };
        if !matches!(op, Op::Generic) {
            self.cpu.rip = insn.next_ip();
            return fast::run(&mut self.cpu, &mut self.mem, op).map_err(|e| Fault(format!("{} (at {rip:#x}: {insn})", e.0)));
        }
        if self.trace {
            let r = &self.cpu.regs;
            let top = self.mem.read_n(r[4], 4).unwrap_or(0xdead_dead);
            eprintln!(
                "{:#x}: {:<28} eax={:08x} ecx={:08x} edx={:08x} ebx={:08x} esp={:08x} ebp={:08x} esi={:08x} edi={:08x} [esp]={:08x}",
                rip, insn.to_string(), r[0] as u32, r[1] as u32, r[2] as u32, r[3] as u32, r[4] as u32, r[5] as u32, r[6] as u32, r[7] as u32, top as u32
            );
        }
        self.cpu.rip = insn.next_ip();
        if self.prof.is_some() {
            let t = tsc();
            let r = self.run_insn(&insn);
            let d = tsc() - t;
            let e = self.prof.as_mut().unwrap().entry(insn.mnemonic() as u32).or_insert((0, 0));
            e.0 += 1;
            e.1 += d;
            return r.map_err(|e| Fault(format!("{} (at {rip:#x}: {insn})", e.0)));
        }
        self.run_insn(&insn).map_err(|e| Fault(format!("{} (at {rip:#x}: {insn})", e.0)))
    }

    pub(crate) fn run_insn(&mut self, insn: &Instruction) -> R<()> {
        // x87（F2xm1〜Fyl2xp1 の範囲）が大半のコード（古いリバーブやフィルター）では、整数と SSE の判定を 2 回通るのが無駄なので、先に x87 を試す。
        // 範囲の外の命令は、整数 → SSE の順（x87 は飛ばす）
        let m = insn.mnemonic() as u32;
        if (158..=265).contains(&m) {
            if self.x87(insn)? {
                return Ok(());
            }
            if exec::exec(&mut self.cpu, &mut self.mem, insn)? || sse::exec(&mut self.cpu, &mut self.mem, insn)? || crate::sse2::exec(&mut self.cpu, &mut self.mem, insn)? {
                return Ok(());
            }
        } else if exec::exec(&mut self.cpu, &mut self.mem, insn)? || sse::exec(&mut self.cpu, &mut self.mem, insn)? || crate::sse2::exec(&mut self.cpu, &mut self.mem, insn)? {
            return Ok(());
        } else if self.x87(insn)? {
            // 範囲の外の x87 の命令（wait など）
            return Ok(());
        }
        if insn.mnemonic() == iced_x86::Mnemonic::Cpuid {
            self.cpuid();
            return Ok(());
        }
        Err(Fault(format!("unimplemented {insn} @ {:#x}", insn.ip())))
    }

    /// cpuid。エミュレーターが実行できるもの（x87、CMOV、CX8、SSE、SSE2）だけを名乗る。AVX などは無いと答え、プラグインに使わせない
    fn cpuid(&mut self) {
        let leaf = self.cpu.regs[0] as u32;
        let (a, b, c, d): (u32, u32, u32, u32) = match leaf {
            // 最大の番号と "GenuineIntel"
            0 => (1, 0x756e_6547, 0x6c65_746e, 0x4965_6e69),
            // family 6。edx: FPU(0)、TSC(4)、CX8(8)、CMOV(15)、MMX(23)、FXSR(24)、SSE(25)、SSE2(26)
            1 => (0x0000_06f1, 0, 0, (1 << 0) | (1 << 4) | (1 << 8) | (1 << 15) | (1 << 23) | (1 << 24) | (1 << 25) | (1 << 26)),
            _ => (0, 0, 0, 0),
        };
        self.cpu.regs[0] = a as u64;
        self.cpu.regs[3] = b as u64;
        self.cpu.regs[1] = c as u64;
        self.cpu.regs[2] = d as u64;
    }

    /// x87 の命令を実行する。環境変数 VSTBRIDGE_NAN があれば、最初に NaN か inf になった命令を報告する
    fn x87(&mut self, insn: &Instruction) -> R<bool> {
        if !self.nan_watch {
            return crate::x87::exec(&mut self.cpu, &mut self.mem, insn);
        }
        let before: Vec<f64> = (0..8).map(|n| self.cpu.st(n)).collect();
        let done = crate::x87::exec(&mut self.cpu, &mut self.mem, insn)?;
        if done && !before.iter().any(|v| !v.is_finite()) && (0..8).any(|n| !self.cpu.st(n).is_finite()) {
            eprintln!("[nan] 最初に NaN か inf になった x87 命令: {insn} @ {:#x}
  前: {:?}
  後: {:?}", insn.ip(), before, (0..8).map(|n| self.cpu.st(n)).collect::<Vec<_>>());
            self.nan_watch = false;
        }
        Ok(done)
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

/// 調査用の時刻（CPU のサイクル。x86_64 以外では 0）
#[cfg(target_arch = "x86_64")]
fn tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}
#[cfg(not(target_arch = "x86_64"))]
fn tsc() -> u64 {
    0
}
