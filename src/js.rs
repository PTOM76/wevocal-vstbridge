//! wasm-bindgen の窓口。JS（ブラウザ、Node）から VST2 の DLL を読み込み、名前、つまみ、音の処理を使えるようにする
use crate::vst::Vst;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct JsVst(Vst);

#[wasm_bindgen]
impl JsVst {
    /// DLL のバイト列から読み込む
    #[wasm_bindgen(constructor)]
    pub fn new(dll: &[u8]) -> Result<JsVst, JsError> {
        Vst::load(dll).map(JsVst).map_err(|e| JsError::new(&e.0))
    }

    pub fn name(&mut self) -> Result<String, JsError> {
        self.0.name().map_err(|e| JsError::new(&e.0))
    }

    #[wasm_bindgen(getter, js_name = numParams)]
    pub fn num_params(&self) -> i32 {
        self.0.num_params
    }

    #[wasm_bindgen(getter, js_name = numInputs)]
    pub fn num_inputs(&self) -> i32 {
        self.0.num_inputs
    }

    #[wasm_bindgen(getter, js_name = numOutputs)]
    pub fn num_outputs(&self) -> i32 {
        self.0.num_outputs
    }

    #[wasm_bindgen(js_name = setParam)]
    pub fn set_param(&mut self, index: i32, value: f32) -> Result<(), JsError> {
        self.0.set_param(index, value).map_err(|e| JsError::new(&e.0))
    }

    #[wasm_bindgen(js_name = getParam)]
    pub fn get_param(&mut self, index: i32) -> Result<f32, JsError> {
        self.0.get_param(index).map_err(|e| JsError::new(&e.0))
    }

    /// 入力（チャンネルごとの Float32Array を並べた配列）を通し、出力（同じ形）を返す
    pub fn process(&mut self, input: Vec<js_sys::Float32Array>, frames: usize) -> Result<Vec<js_sys::Float32Array>, JsError> {
        let input: Vec<Vec<f32>> = input.iter().map(|a| a.to_vec()).collect();
        let out = self.0.process(&input, frames).map_err(|e| JsError::new(&e.0))?;
        Ok(out.iter().map(|c| js_sys::Float32Array::from(c.as_slice())).collect())
    }
}
