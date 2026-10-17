//! JIT。よく通る x86 のブロックを wasm の関数に変換して動かす（設計は memo/jit.md）。変換は gen.rs、読み込みは install.rs（wasm の中だけ）
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
