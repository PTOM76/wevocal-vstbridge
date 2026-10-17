//! 生成した wasm を、この wasm と同じ memory と関数の表につないで動かせるようにする（wasm の中だけ）。
//! 出来た関数は表に足し、その添字を関数ポインターとして Rust から直接呼ぶ（JS を通らない）
use js_sys::{Object, Reflect, Uint8Array, WebAssembly};
use wasm_bindgen::{JsCast, JsValue};

/// 生成した関数の形（引数は Cpu の番地、戻り値は実行した命令の数）
pub type JitFn = extern "C" fn(u32) -> u32;

/// bytes（1 つの関数 "f" を書き出す wasm）を読み込み、表に足して呼べる形にする
pub fn install(bytes: &[u8]) -> Result<JitFn, String> {
    let err = |e: JsValue| format!("{e:?}");
    let module = WebAssembly::Module::new(&Uint8Array::from(bytes)).map_err(err)?;
    let env = Object::new();
    Reflect::set(&env, &"memory".into(), &wasm_bindgen::memory()).map_err(err)?;
    let table: WebAssembly::Table = wasm_bindgen::function_table().unchecked_into();
    Reflect::set(&env, &"table".into(), &table).map_err(err)?;
    let imports = Object::new();
    Reflect::set(&imports, &"env".into(), &env).map_err(err)?;
    let inst = WebAssembly::Instance::new(&module, &imports).map_err(err)?;
    let f = Reflect::get(&inst.exports(), &"f".into()).map_err(err)?;
    let idx = table.grow(1).map_err(err)?;
    table.set(idx, f.unchecked_ref()).map_err(err)?;
    // SAFETY: wasm32 の関数ポインターは関数の表の添字。f の型は (i32) -> i32 で JitFn と同じ
    Ok(unsafe { std::mem::transmute::<usize, JitFn>(idx as usize) })
}
