//! wasm-bindgen の窓口。JS（ブラウザ、Node）から VST2 の DLL を読み込み、名前、つまみ、音の処理を使えるようにする。
//! DLL が 32 ビットか 64 ビットかは PE ヘッダーで見分け、どちらも同じ JsVst として扱う
use crate::vst::Vst;
use crate::vst32::Vst32;
use wasm_bindgen::prelude::*;

enum Plugin {
    X64(Vst),
    X86(Vst32),
}

/// PE ヘッダーの Machine（0x14c = x86、0x8664 = x64）を読む。PE でなければ None
fn machine(dll: &[u8]) -> Option<u16> {
    let pe = u32::from_le_bytes(dll.get(0x3c..0x40)?.try_into().ok()?) as usize;
    if dll.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    Some(u16::from_le_bytes(dll.get(pe + 4..pe + 6)?.try_into().ok()?))
}

#[wasm_bindgen]
pub struct JsVst(Plugin);

#[wasm_bindgen]
impl JsVst {
    /// DLL のバイト列から読み込む（32 ビットも 64 ビットも）
    #[wasm_bindgen(constructor)]
    pub fn new(dll: &[u8]) -> Result<JsVst, JsError> {
        match machine(dll) {
            Some(0x14c) => Vst32::load(dll).map(|v| JsVst(Plugin::X86(v))).map_err(|e| JsError::new(&e.0)),
            Some(0x8664) => Vst::load(dll).map(|v| JsVst(Plugin::X64(v))).map_err(|e| JsError::new(&e.0)),
            Some(m) => Err(JsError::new(&format!("未対応の CPU です（Machine={m:#x}）"))),
            None => Err(JsError::new("DLL（PE）ではありません")),
        }
    }

    /// 32 ビットの DLL か
    #[wasm_bindgen(getter, js_name = is32bit)]
    pub fn is_32bit(&self) -> bool {
        matches!(self.0, Plugin::X86(_))
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
}
