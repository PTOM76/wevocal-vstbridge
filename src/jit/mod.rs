//! JIT。よく通る x86 のブロックを wasm の関数に変換して動かす（設計は memo/jit.md）。変換は gen.rs、読み込みは install.rs（wasm の中だけ）
pub mod codegen;
#[cfg(target_arch = "wasm32")]
pub mod install;

use wasm_encoder::{CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection, ImportSection, Instruction as W, MemoryType, Module, RefType, TableType, TypeSection, ValType};

/// 1 つの関数 "f"（(i32) -> i32）だけを書き出すモジュールを作る。memory と table を env から import する
pub fn module(locals: &[(u32, ValType)], body: &[W]) -> Vec<u8> {
    let mut m = Module::new();
    let mut types = TypeSection::new();
    types.ty().function([ValType::I32], [ValType::I32]);
    // 1: 遅い道の Rust の関数（cpu, 番号）。表の添字で call_indirect する
    types.ty().function([ValType::I32, ValType::I32], []);
    m.section(&types);
    let mut imports = ImportSection::new();
    imports.import("env", "memory", EntityType::Memory(MemoryType { minimum: 1, maximum: None, memory64: false, shared: false, page_size_log2: None }));
    imports.import("env", "table", EntityType::Table(TableType { element_type: RefType::FUNCREF, minimum: 1, maximum: None, table64: false, shared: false }));
    m.section(&imports);
    let mut funcs = FunctionSection::new();
    funcs.function(0);
    m.section(&funcs);
    let mut exports = ExportSection::new();
    exports.export("f", ExportKind::Func, 0);
    m.section(&exports);
    let mut code = CodeSection::new();
    let mut f = Function::new(locals.iter().copied());
    for i in body {
        f.instruction(i);
    }
    f.instruction(&W::End);
    code.function(&f);
    m.section(&code);
    m.finish()
}

/// JIT の遅い道: x87 の超越関数など（k は x87::TRANSCEND の添字）。生成したコードが表の添字で呼ぶ
pub extern "C" fn x87_helper(cpu: u32, k: u32) {
    // SAFETY: 生成したコードは、動いている Emu の Cpu の番地を渡す
    let cpu = unsafe { &mut *(cpu as usize as *mut crate::cpu::Cpu) };
    crate::x87::transcend(cpu, crate::x87::TRANSCEND[k as usize]);
}

/// JIT の遅い道: fld tbyte。hp は 10 バイトのホストの番地
pub extern "C" fn f80_load(cpu: u32, hp: u32) {
    // SAFETY: x87_helper と同じ。hp は生成したコードがページ表で引いた、10 バイトが読める番地
    let (cpu, b) = unsafe { (&mut *(cpu as usize as *mut crate::cpu::Cpu), *(hp as usize as *const [u8; 10])) };
    cpu.fpush(crate::f80::to_f64(b));
}

/// JIT の遅い道: fstp tbyte
pub extern "C" fn f80_store_pop(cpu: u32, hp: u32) {
    // SAFETY: f80_load と同じ
    let cpu = unsafe { &mut *(cpu as usize as *mut crate::cpu::Cpu) };
    unsafe { *(hp as usize as *mut [u8; 10]) = crate::f80::from_f64(cpu.st(0)) };
    cpu.fpop();
}

/// つなぎ方の確かめ: 引数に 42 を足して返す関数を作って呼ぶ
#[cfg(target_arch = "wasm32")]
pub fn selftest(x: u32) -> Result<u32, String> {
    let bytes = module(&[], &[W::LocalGet(0), W::I32Const(42), W::I32Add]);
    let f = install::install(&bytes)?;
    Ok(f(x))
}

/// 変換したブロックを覚えて動かす（wasm の中だけ）。ネイティブでは実行できないので、いつも覚えないまま（インタープリター）
pub struct Jit {
    /// ブロックの番地 → 変換した関数（None は「変換できない」と分かったもの）
    #[cfg(target_arch = "wasm32")]
    funcs: std::collections::HashMap<u64, Option<install::JitFn>, crate::emu::FastHash>,
    /// 番地ごとの実行回数（この回数を超えたら変換する）
    hits: std::collections::HashMap<u64, u32, crate::emu::FastHash>,
    /// 変換を始める回数
    threshold: u32,
    pub on: bool,
    /// インタープリターで動かした命令を jit_miss に数えるか（調査用。数えると遅くなる）
    pub count_miss: bool,
}

impl Default for Jit {
    fn default() -> Self {
        let threshold = std::env::var("VSTBRIDGE_JIT_THRESHOLD").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
        Jit {
            #[cfg(target_arch = "wasm32")]
            funcs: Default::default(),
            hits: Default::default(),
            threshold,
            on: std::env::var("VSTBRIDGE_NOJIT").is_err(),
            count_miss: false,
        }
    }
}

impl Jit {
    pub fn clear(&mut self) {
        #[cfg(target_arch = "wasm32")]
        self.funcs.clear();
        self.hits.clear();
    }
}

#[cfg(target_arch = "wasm32")]
impl Jit {
    /// rip のブロックの変換済み関数。まだなら、回数を数え、十分なら変換する
    pub fn get(&mut self, mem: &crate::mem::Memory, bits: u32, rip: u64, max_block: usize) -> Option<install::JitFn> {
        if let Some(f) = self.funcs.get(&rip) {
            return *f;
        }
        let h = self.hits.entry(rip).or_insert(0);
        *h += 1;
        if *h < self.threshold {
            return None;
        }
        let f = codegen::compile_block(mem, bits, rip, max_block).and_then(|c| {
            let bytes = module(&c.locals, &c.body);
            install::install(&bytes).ok()
        });
        self.funcs.insert(rip, f);
        f
    }
}
