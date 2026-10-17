//! VST2 の DLL を読み込み、エミュレーターの上で動かす（音を通す）。testplug/gain.dll で確かめる
use crate::emu::{Arg, Emu};
use crate::mem::{Fault, R};
use crate::pe;

/// AEffect 構造体の中の位置（vst2.h の並び。x64 のそろえ方で計算）
mod off {
    pub const MAGIC: u64 = 0;
    pub const DISPATCHER: u64 = 8;
    pub const SET_PARAMETER: u64 = 24;
    pub const GET_PARAMETER: u64 = 32;
    pub const NUM_PARAMS: u64 = 44;
    pub const NUM_INPUTS: u64 = 48;
    pub const NUM_OUTPUTS: u64 = 52;
    pub const PROCESS_REPLACING: u64 = 120;
}

const VST_MAGIC: u64 = 0x5673_7450; // 'VstP'
const EFF_GET_PARAM_NAME: u64 = 8;
const EFF_GET_EFFECT_NAME: u64 = 45;

pub struct Vst {
    pub emu: Emu,
    /// AEffect 構造体の番地
    pub effect: u64,
    pub num_params: i32,
    pub num_inputs: i32,
    pub num_outputs: i32,
    /// 音を通すときのバッファ（ブロックごとに確保し直さない）
    io: Option<Io>,
}

struct Io {
    frames: usize,
    in_ptrs: u64,
    in_bufs: Vec<u64>,
    out_ptrs: u64,
    out_bufs: Vec<u64>,
}

impl Vst {
    /// DLL のバイト列から読み込み、VSTPluginMain を呼んで AEffect を受け取る
    pub fn load(dll: &[u8]) -> R<Vst> {
        let mut emu = Emu::new();
        let img = pe::load(&mut emu.mem, dll)?;
        emu.image_base = img.base;
        emu.gui.base = img.base;
        pe::patch_imports(&mut emu, dll, img.base, crate::host_stubs::resolve, crate::host_stubs::unknown)?;
        let entry = img.export("VSTPluginMain").or_else(|| img.export("main")).ok_or_else(|| Fault("VSTPluginMain が無い".into()))?;

        // ホストへの呼び出し（audioMasterCallback）。opcode は rdx（第 2 引数）。audioMasterVersion(1) には 2400 を返す
        let host = emu.add_thunk(Box::new(|e: &mut Emu| {
            let opcode = e.cpu.regs[2]; // rdx
            Ok(if opcode == 1 { 2400 } else { 0 })
        }));

        // TLS の領域を作り、コールバックを DllMain と同じ引数で呼ぶ
        for cb in pe::setup_tls(&mut emu, dll, img.base)? {
            emu.call(cb, &[Arg::Int(img.base), Arg::Int(1), Arg::Int(0)])?;
        }
        // DLL の入口（DllMainCRTStartup）を先に呼ぶ（静的な初期化のため。無くても動くものもある）
        if img.entry != img.base {
            emu.call(img.entry, &[Arg::Int(img.base), Arg::Int(1), Arg::Int(0)])?;
        }

        let effect = emu.call(entry, &[Arg::Int(host)])?;
        if effect == 0 {
            return Err(Fault("VSTPluginMain が 0 を返した".into()));
        }
        let magic = emu.mem.read_n(effect + off::MAGIC, 4)?;
        if magic != VST_MAGIC {
            return Err(Fault(format!("magic が違う: {magic:#x}")));
        }
        let num_params = emu.mem.read_n(effect + off::NUM_PARAMS, 4)? as i32;
        let num_inputs = emu.mem.read_n(effect + off::NUM_INPUTS, 4)? as i32;
        let num_outputs = emu.mem.read_n(effect + off::NUM_OUTPUTS, 4)? as i32;
        Ok(Vst { emu, effect, num_params, num_inputs, num_outputs, io: None })
    }

    fn ptr(&self, field: u64) -> R<u64> {
        self.emu.mem.read_n(self.effect + field, 8)
    }

    /// 名前を取る（dispatcher(effGetEffectName, 0, 0, buf, 0)。buf に文字が入る）
    pub fn name(&mut self) -> R<String> {
        let dispatcher = self.ptr(off::DISPATCHER)?;
        if dispatcher == 0 {
            return Ok(String::new());
        }
        let buf = self.emu.mem.alloc(64);
        // dispatcher(effect, opcode, index, value, ptr, opt)
        self.emu.call(dispatcher, &[Arg::Int(self.effect), Arg::Int(EFF_GET_EFFECT_NAME), Arg::Int(0), Arg::Int(0), Arg::Int(buf), Arg::F32(0.0)])?;
        self.emu.mem.read_cstr(buf, 64)
    }

    /// つまみの名前（dispatcher(effGetParamName, index, 0, buf, 0)）。dispatcher が無ければ "Param n"
    pub fn param_name(&mut self, index: i32) -> R<String> {
        let dispatcher = self.ptr(off::DISPATCHER)?;
        if dispatcher == 0 {
            return Ok(format!("Param {index}"));
        }
        let buf = self.emu.mem.alloc(64);
        self.emu.call(dispatcher, &[Arg::Int(self.effect), Arg::Int(EFF_GET_PARAM_NAME), Arg::Int(index as u64), Arg::Int(0), Arg::Int(buf), Arg::F32(0.0)])?;
        let name = self.emu.mem.read_cstr(buf, 64)?;
        Ok(if name.is_empty() { format!("Param {index}") } else { name })
    }

    pub fn set_param(&mut self, index: i32, value: f32) -> R<()> {
        let f = self.ptr(off::SET_PARAMETER)?;
        self.emu.call(f, &[Arg::Int(self.effect), Arg::Int(index as u64), Arg::F32(value)])?;
        Ok(())
    }

    pub fn get_param(&mut self, index: i32) -> R<f32> {
        let f = self.ptr(off::GET_PARAMETER)?;
        let _ = self.emu.call(f, &[Arg::Int(self.effect), Arg::Int(index as u64)])?;
        // 戻り値は xmm0（f32）
        Ok(f32::from_bits(self.emu.cpu.xmm[0] as u32))
    }

    /// 音を通す（入力の各チャンネル → 出力の各チャンネル。processReplacing(effect, in**, out**, n)）
    pub fn process(&mut self, input: &[Vec<f32>], frames: usize) -> R<Vec<Vec<f32>>> {
        let ni = self.num_inputs.max(0) as usize;
        let no = self.num_outputs.max(0) as usize;
        // バッファは、ブロックの長さが変わったときだけ確保し直す
        if self.io.as_ref().map(|io| io.frames) != Some(frames) {
            let mem = &mut self.emu.mem;
            let in_ptrs = mem.alloc(ni * 8);
            let mut in_bufs = Vec::with_capacity(ni);
            for c in 0..ni {
                let buf = mem.alloc(frames * 4);
                mem.write_n(in_ptrs + c as u64 * 8, 8, buf)?;
                in_bufs.push(buf);
            }
            let out_ptrs = mem.alloc(no * 8);
            let mut out_bufs = Vec::with_capacity(no);
            for c in 0..no {
                let buf = mem.alloc(frames * 4);
                mem.write_n(out_ptrs + c as u64 * 8, 8, buf)?;
                out_bufs.push(buf);
            }
            self.io = Some(Io { frames, in_ptrs, in_bufs, out_ptrs, out_bufs });
        }
        let io = self.io.as_ref().unwrap();
        for (c, buf) in io.in_bufs.iter().enumerate() {
            let src = input.get(c).map(|v| v.as_slice()).unwrap_or(&[]);
            let mut data = vec![0f32; frames];
            data[..src.len().min(frames)].copy_from_slice(&src[..src.len().min(frames)]);
            self.emu.mem.write_f32s(*buf, &data)?;
        }
        let (in_ptrs, out_ptrs, out_bufs) = (io.in_ptrs, io.out_ptrs, io.out_bufs.clone());
        let process = self.ptr(off::PROCESS_REPLACING)?;
        if process == 0 {
            return Err(Fault("processReplacing が無い".into()));
        }
        self.emu.call(process, &[Arg::Int(self.effect), Arg::Int(in_ptrs), Arg::Int(out_ptrs), Arg::Int(frames as u64)])?;
        let mut out = Vec::with_capacity(no);
        for buf in out_bufs {
            out.push(self.emu.mem.read_f32s(buf, frames)?);
        }
        Ok(out)
    }
}
