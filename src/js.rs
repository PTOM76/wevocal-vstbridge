//! wasm-bindgen の窓口。JS（ブラウザ、Node）から VST2 の DLL を読み込み、名前、つまみ、音の処理を使えるようにする。
//! DLL が 32 ビットか 64 ビットかは PE ヘッダーで見分け、どちらも同じ JsVst として扱う
use crate::loader::{Loaded, Loader};
use crate::vst::Vst;
use crate::vst32::Vst32;
use crate::vst32_gui::Editor;
use wasm_bindgen::prelude::*;

enum Plugin {
    X64(Vst),
    X86(Vst32),
}

#[wasm_bindgen]
pub struct JsVst(Plugin);

impl From<Loaded> for JsVst {
    fn from(l: Loaded) -> Self {
        JsVst(match l {
            Loaded::X64(v) => Plugin::X64(v),
            Loaded::X86(v) => Plugin::X86(v),
        })
    }
}

/// DLL の読み込みを少しずつ進める（AudioWorklet の中で、音のスレッドを長く止めないため）。
/// new で置いたあと、step を終わる（true）まで呼び、finish で JsVst にする
#[wasm_bindgen]
pub struct JsVstLoader(Loader);

#[wasm_bindgen]
impl JsVstLoader {
    /// DLL を置き、import をつなぐ（命令はまだ動かさない）
    #[wasm_bindgen(constructor)]
    pub fn new(dll: &[u8]) -> Result<JsVstLoader, JsError> {
        Loader::new(dll).map(JsVstLoader).map_err(|e| JsError::new(&e.0))
    }

    /// 命令を最大 budget 個動かして読み込みを進める。終わったら true
    pub fn step(&mut self, budget: f64) -> Result<bool, JsError> {
        self.0.step(budget as u64).map_err(|e| JsError::new(&e.0))
    }

    /// 読み込みが終わったものを JsVst にする（この loader は使えなくなる）
    pub fn finish(self) -> Result<JsVst, JsError> {
        self.0.finish().map(JsVst::from).map_err(|e| JsError::new(&e.0))
    }
}

#[wasm_bindgen]
impl JsVst {
    /// DLL のバイト列から読み込む（32 ビットも 64 ビットも）
    #[wasm_bindgen(constructor)]
    pub fn new(dll: &[u8]) -> Result<JsVst, JsError> {
        Loader::load(dll).map(JsVst::from).map_err(|e| JsError::new(&e.0))
    }

    /// wasm のメモリの写しから戻したあとに呼ぶ。変換した関数（JIT）は元の wasm の関数の表にあり、戻した先には無いので捨てる（使うとまた変換する）。
    /// 写しは、読み込みの済んだ JsVst があるメモリを、JS から何も呼んでいない間にまるごと写したもの（同じ wasm に限る）
    #[wasm_bindgen(js_name = afterRestore)]
    pub fn after_restore(&mut self) {
        match &mut self.0 {
            Plugin::X86(v) => v.emu.jit.clear(),
            Plugin::X64(v) => v.emu.jit.clear(),
        }
        #[cfg(target_arch = "wasm32")]
        crate::jit::install::reset_slots();
    }

    /// 32 ビットの DLL か
    #[wasm_bindgen(getter, js_name = is32bit)]
    pub fn is_32bit(&self) -> bool {
        matches!(self.0, Plugin::X86(_))
    }

    /// JIT で実行した命令の割合（調査用。0..1）
    #[wasm_bindgen(js_name = jitCoverage)]
    pub fn jit_coverage(&self) -> f64 {
        let e = match &self.0 {
            Plugin::X86(v) => &v.emu,
            Plugin::X64(v) => &v.emu,
        };
        let t = e.jit_insns + e.interp_insns;
        if t == 0 { 0.0 } else { e.jit_insns as f64 / t as f64 }
    }

    /// JIT で変換しない命令の名前（カンマ区切り、小文字。例 "bts,imul"。JIT の不具合を命令の種類で切り分けるため。調査用）
    #[wasm_bindgen(js_name = setJitDeny)]
    pub fn set_jit_deny(&mut self, names: &str) {
        crate::jit::codegen::DENY.with(|d| *d.borrow_mut() = names.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect());
    }

    /// jitMisses のために数え始める（調査用。数えると遅くなる）
    #[wasm_bindgen(js_name = countJitMisses)]
    pub fn count_jit_misses(&mut self) {
        match &mut self.0 {
            Plugin::X86(v) => v.emu.jit.count_miss = true,
            Plugin::X64(v) => v.emu.jit.count_miss = true,
        }
    }

    /// JIT が有効なのにインタープリターで動かしたブロックの先頭の命令（多い順に "回数 命令" の行。調査用。先に countJitMisses）
    #[wasm_bindgen(js_name = jitMisses)]
    pub fn jit_misses(&self) -> String {
        let e = match &self.0 {
            Plugin::X86(v) => &v.emu,
            Plugin::X64(v) => &v.emu,
        };
        let mut v: Vec<_> = e.jit_miss.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        v.iter().take(30).map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join("
")
    }

    /// 変換を始める実行回数（既定 8。調査用）
    #[wasm_bindgen(js_name = setJitThreshold)]
    pub fn set_jit_threshold(&mut self, n: u32) {
        match &mut self.0 {
            Plugin::X86(v) => v.emu.jit.threshold = n,
            Plugin::X64(v) => v.emu.jit.threshold = n,
        }
    }

    /// 変換した関数の数、変換にかかった時間、生成した wasm の合計、JIT の関数を呼んだ回数、JIT で動いた命令の数（"数 ms バイト 回数 命令数"。調査用）
    #[wasm_bindgen(js_name = jitStats)]
    pub fn jit_stats(&self) -> String {
        let e = match &self.0 {
            Plugin::X86(v) => &v.emu,
            Plugin::X64(v) => &v.emu,
        };
        let j = &e.jit;
        format!("{} {:.0} {} {} {}", j.compiled, j.compile_ms, j.compiled_bytes, e.jit_calls, e.jit_insns)
    }

    /// JIT を使うか（ありとなしで出力を比べるため。既定は使う）
    #[wasm_bindgen(js_name = setJit)]
    pub fn set_jit(&mut self, on: bool) {
        let e = match &mut self.0 {
            Plugin::X86(v) => &mut v.emu,
            Plugin::X64(v) => &mut v.emu,
        };
        e.jit.on = on;
        e.jit.clear();
    }

    pub fn name(&mut self) -> Result<String, JsError> {
        match &mut self.0 {
            Plugin::X64(v) => v.name(),
            Plugin::X86(v) => v.name(),
        }
        .map_err(|e| JsError::new(&e.0))
    }

    #[wasm_bindgen(getter, js_name = numParams)]
    pub fn num_params(&self) -> i32 {
        match &self.0 {
            Plugin::X64(v) => v.num_params,
            Plugin::X86(v) => v.num_params,
        }
    }

    #[wasm_bindgen(getter, js_name = numInputs)]
    pub fn num_inputs(&self) -> i32 {
        match &self.0 {
            Plugin::X64(v) => v.num_inputs,
            Plugin::X86(v) => v.num_inputs,
        }
    }

    #[wasm_bindgen(getter, js_name = numOutputs)]
    pub fn num_outputs(&self) -> i32 {
        match &self.0 {
            Plugin::X64(v) => v.num_outputs,
            Plugin::X86(v) => v.num_outputs,
        }
    }

    /// つまみ index の名前
    #[wasm_bindgen(js_name = paramName)]
    pub fn param_name(&mut self, index: i32) -> Result<String, JsError> {
        match &mut self.0 {
            Plugin::X64(v) => v.param_name(index),
            Plugin::X86(v) => v.param_name(index),
        }
        .map_err(|e| JsError::new(&e.0))
    }

    #[wasm_bindgen(js_name = setParam)]
    pub fn set_param(&mut self, index: i32, value: f32) -> Result<(), JsError> {
        match &mut self.0 {
            Plugin::X64(v) => v.set_param(index, value),
            Plugin::X86(v) => v.set_param(index, value),
        }
        .map_err(|e| JsError::new(&e.0))
    }

    #[wasm_bindgen(js_name = getParam)]
    pub fn get_param(&mut self, index: i32) -> Result<f32, JsError> {
        match &mut self.0 {
            Plugin::X64(v) => v.get_param(index),
            Plugin::X86(v) => v.get_param(index),
        }
        .map_err(|e| JsError::new(&e.0))
    }

    /// MIDI のメッセージ（status、data1、data2）を送る。次の process のブロックの頭から delta サンプル目に効く
    #[wasm_bindgen(js_name = sendMidi)]
    pub fn send_midi(&mut self, status: u8, data1: u8, data2: u8, delta: i32) -> Result<(), JsError> {
        match &mut self.0 {
            Plugin::X64(v) => v.send_midi([status, data1, data2], delta),
            Plugin::X86(v) => v.send_midi([status, data1, data2], delta),
        }
        .map_err(|e| JsError::new(&e.0))
    }

    /// 入力（チャンネルごとの Float32Array を並べた配列）を通し、出力（同じ形）を返す
    pub fn process(&mut self, input: Vec<js_sys::Float32Array>, frames: usize) -> Result<Vec<js_sys::Float32Array>, JsError> {
        let input: Vec<Vec<f32>> = input.iter().map(|a| a.to_vec()).collect();
        let out = match &mut self.0 {
            Plugin::X64(v) => v.process(&input, frames),
            Plugin::X86(v) => v.process(&input, frames),
        }
        .map_err(|e| JsError::new(&e.0))?;
        Ok(out.iter().map(|c| js_sys::Float32Array::from(c.as_slice())).collect())
    }

    // ---- エディター（プラグイン自身の画面）。今は 32 ビットだけ ----

    /// 画面を持つか
    #[wasm_bindgen(js_name = hasEditor)]
    pub fn has_editor(&mut self) -> bool {
        match &mut self.0 {
            Plugin::X86(v) => v.has_editor().unwrap_or(false),
            Plugin::X64(v) => v.has_editor().unwrap_or(false),
        }
    }

    /// 画面を開く。[幅, 高さ]
    #[wasm_bindgen(js_name = editorOpen)]
    pub fn editor_open(&mut self) -> Result<Vec<i32>, JsError> {
        match &mut self.0 {
            Plugin::X86(v) => v.editor_open().map(|(w, h)| vec![w, h]).map_err(|e| JsError::new(&e.0)),
            Plugin::X64(v) => v.editor_open().map(|(w, h)| vec![w, h]).map_err(|e| JsError::new(&e.0)),
        }
    }

    /// マウスの入力。msg は 0x200 移動、0x201 左ボタンを押した、0x202 離した、0x203 ダブルクリック。keys は bit0 左ボタン、bit1 Ctrl、bit2 Shift
    #[wasm_bindgen(js_name = editorMouse)]
    pub fn editor_mouse(&mut self, msg: u32, x: i32, y: i32, keys: u32) -> Result<(), JsError> {
        match &mut self.0 {
            Plugin::X86(v) => v.editor_mouse(msg, x, y, keys).map_err(|e| JsError::new(&e.0)),
            Plugin::X64(v) => v.editor_mouse(msg, x, y, keys).map_err(|e| JsError::new(&e.0)),
        }
    }

    /// 一定時間ごと（30 回ほど毎秒）に呼ぶ
    #[wasm_bindgen(js_name = editorIdle)]
    pub fn editor_idle(&mut self) -> Result<(), JsError> {
        match &mut self.0 {
            Plugin::X86(v) => v.editor_idle().map_err(|e| JsError::new(&e.0)),
            Plugin::X64(v) => v.editor_idle().map_err(|e| JsError::new(&e.0)),
        }
    }

    /// 今の画面（RGBA。幅と高さは editorOpen で返したもの）
    #[wasm_bindgen(js_name = editorFrame)]
    pub fn editor_frame(&self) -> Option<Vec<u8>> {
        match &self.0 {
            Plugin::X86(v) => v.editor_frame().map(|(_, _, px)| px),
            Plugin::X64(v) => v.editor_frame().map(|(_, _, px)| px),
        }
    }

    /// 画面でつまみを変えた分。[index, 値, index, 値, …]
    #[wasm_bindgen(js_name = editorChanges)]
    pub fn editor_changes(&mut self) -> Vec<f32> {
        match &mut self.0 {
            Plugin::X86(v) => v.editor_changes().into_iter().flat_map(|(i, x)| [i as f32, x]).collect(),
            Plugin::X64(v) => v.editor_changes().into_iter().flat_map(|(i, x)| [i as f32, x]).collect(),
        }
    }

    #[wasm_bindgen(js_name = editorClose)]
    pub fn editor_close(&mut self) -> Result<(), JsError> {
        match &mut self.0 {
            Plugin::X86(v) => v.editor_close().map_err(|e| JsError::new(&e.0)),
            Plugin::X64(v) => v.editor_close().map_err(|e| JsError::new(&e.0)),
        }
    }
}

/// JIT のつなぎ方の確かめ（引数に 42 を足して返す）
#[wasm_bindgen(js_name = jitSelftest)]
pub fn jit_selftest(x: u32) -> Result<u32, JsError> {
    crate::jit::selftest(x).map_err(|e| JsError::new(&e))
}
