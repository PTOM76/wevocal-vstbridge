//! Windows の VST プラグイン（DLL）を、x86-64 のエミュレーターでブラウザ（wasm）でも動かすための研究用のライブラリ。
//! HANDOFF.md と、WeVocal Studio の memo/effects.md の 6 章を参照
pub mod cpu;
pub mod emu;
pub mod exec;
pub mod host_stubs;
#[cfg(target_arch = "wasm32")]
pub mod js;
pub mod mem;
pub mod pe;
pub mod pe32;
pub mod sse;
pub mod vst;
pub mod vst32;
