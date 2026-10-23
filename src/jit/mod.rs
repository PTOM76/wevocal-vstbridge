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
