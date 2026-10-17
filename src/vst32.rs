//! 32 ビットの VST2 の DLL を読み込み、エミュレーターの上で動かす。64 ビット版（vst.rs）と違い、
//! ポインターが 4 バイトなので AEffect の中の位置（オフセット）が変わる。import は host_stubs.rs のスタブで解決する
use crate::emu::{Arg, Emu};
use crate::host_stubs;
use crate::mem::{Fault, R};
use crate::pe32;

/// AEffect 構造体の中の位置（32 ビット。ポインターが 4 バイトなのでずれる）
mod off {
    pub const MAGIC: u64 = 0;
    pub const DISPATCHER: u64 = 4;
    pub const SET_PARAMETER: u64 = 12;
    pub const GET_PARAMETER: u64 = 16;
    pub const NUM_PARAMS: u64 = 24;
    pub const NUM_INPUTS: u64 = 28;
    pub const NUM_OUTPUTS: u64 = 32;
    // 32 ビットの AEffect: offset 64 は object、68 は user、72 uniqueID、76 version、80 processReplacing
    pub const PROCESS_REPLACING: u64 = 80;
}

const VST_MAGIC: u64 = 0x5673_7450; // 'VstP'
const EFF_OPEN: u64 = 0;
const EFF_SET_PROGRAM: u64 = 2;
const EFF_SET_SAMPLE_RATE: u64 = 10;
const EFF_SET_BLOCK_SIZE: u64 = 11;
const EFF_MAINS_CHANGED: u64 = 12;
const EFF_GET_PARAM_NAME: u64 = 8;
const EFF_GET_EFFECT_NAME: u64 = 45;

pub struct Vst32 {
    pub emu: Emu,
    pub effect: u32,
    pub num_params: i32,
    pub num_inputs: i32,
    pub num_outputs: i32,
}

impl Vst32 {
    /// DLL のバイト列から読み込む。import はログに出すだけで、知っているものだけ動く（host_stubs.rs）
    pub fn load(dll: &[u8]) -> R<Vst32> {
        let mut emu = Emu::new32();
        let img = pe32::load(&mut emu, dll, host_stubs::resolve, host_stubs::unknown)?;
        let entry = img.export("VSTPluginMain").or_else(|| img.export("main")).ok_or_else(|| Fault("VSTPluginMain が無い".into()))?;

        // ホストへの呼び出し（audioMasterCallback）。opcode は 2 番目の引数（スタックの上から 2 つ目）
        let host = emu.add_thunk(Box::new(|e: &mut Emu| {
            // audioMasterCallback(effect, opcode, index, value, ptr, opt) は cdecl。戻り先を pop した直後なので esp は effect を指す
            let esp = e.cpu.regs[crate::cpu::RSP];
            let opcode = e.mem.read_n(esp + 4, 4)?;
            if std::env::var("VSTBRIDGE_TRACE").is_ok() {
                eprintln!("[audioMaster] opcode={opcode}");
            }
            // 1=version（2400）、6=wantMidi、ほかは 0。音の処理に関わる要求（サンプルレートなど）はプラグインが dispatcher で受け取る側
            Ok(if opcode == 1 { 2400 } else { 0 })
        }));

        if img.entry != img.base {
            emu.call32(img.entry, &[Arg::Int(img.base as u64), Arg::Int(1), Arg::Int(0)])?;
        }

        let effect = emu.call32(entry, &[Arg::Int(host)])? as u32;
        if effect == 0 {
            return Err(Fault("VSTPluginMain が 0 を返した".into()));
        }
        let magic = emu.mem.read_n(effect as u64 + off::MAGIC, 4)?;
        if magic != VST_MAGIC {
            return Err(Fault(format!("magic が違う: {magic:#x}")));
        }
        let num_params = emu.mem.read_n(effect as u64 + off::NUM_PARAMS, 4)? as i32;
        let num_inputs = emu.mem.read_n(effect as u64 + off::NUM_INPUTS, 4)? as i32;
        let num_outputs = emu.mem.read_n(effect as u64 + off::NUM_OUTPUTS, 4)? as i32;
        let mut v = Vst32 { emu, effect, num_params, num_inputs, num_outputs };
        // ホストが普通に行う初期化（開く、サンプルレート、ブロックサイズ、電源オン）。これをしないと内部バッファが未初期化のまま
        v.dispatch(EFF_OPEN, 0, 0, 0, 0.0)?;
        v.dispatch(EFF_SET_PROGRAM, 0, 0, 0, 0.0)?;
        v.dispatch(EFF_SET_SAMPLE_RATE, 0, 0, 0, 48000.0)?;
        v.dispatch(EFF_SET_BLOCK_SIZE, 0, 2048, 0, 0.0)?; // ブロックサイズは value で渡す（ptr ではない）
        v.dispatch(EFF_MAINS_CHANGED, 0, 1, 0, 0.0)?;
        Ok(v)
    }

    /// dispatcher(effect, opcode, index, value, ptr, opt) を呼ぶ
    fn dispatch(&mut self, opcode: u64, index: u64, value: u64, ptr: u64, opt: f32) -> R<u64> {
        let f = self.ptr(off::DISPATCHER)?;
        if f == 0 {
            return Ok(0);
        }
        self.emu.call32(f, &[Arg::Int(self.effect as u64), Arg::Int(opcode), Arg::Int(index), Arg::Int(value), Arg::Int(ptr), Arg::F32(opt)])
    }

    fn ptr(&self, field: u64) -> R<u32> {
        Ok(self.emu.mem.read_n(self.effect as u64 + field, 4)? as u32)
    }

    /// つまみの名前（dispatcher(effGetParamName, index, 0, buf, 0)）。dispatcher が無ければ "Param n"
    pub fn param_name(&mut self, index: i32) -> R<String> {
        let dispatcher = self.ptr(off::DISPATCHER)?;
        if dispatcher == 0 {
            return Ok(format!("Param {index}"));
        }
        let buf = self.emu.mem.alloc32(64);
        self.emu.call32(dispatcher, &[Arg::Int(self.effect as u64), Arg::Int(EFF_GET_PARAM_NAME), Arg::Int(index as u64), Arg::Int(0), Arg::Int(buf as u64), Arg::F32(0.0)])?;
        let name = self.emu.mem.read_cstr(buf as u64, 64)?;
        Ok(if name.is_empty() { format!("Param {index}") } else { name })
    }

    pub fn set_param(&mut self, index: i32, value: f32) -> R<()> {
        let f = self.ptr(off::SET_PARAMETER)?;
        self.emu.call32(f, &[Arg::Int(self.effect as u64), Arg::Int(index as u64), Arg::F32(value)])?;
        Ok(())
    }

    pub fn get_param(&mut self, index: i32) -> R<f32> {
        let f = self.ptr(off::GET_PARAMETER)?;
        self.emu.call32(f, &[Arg::Int(self.effect as u64), Arg::Int(index as u64)])?;
        // 32 ビットの VST は戻り値の f32 を x87 の st(0) で返す
        Ok(self.emu.cpu.st(0) as f32)
    }

    /// 音を通す（processReplacing(effect, in**, out**, n)）。ポインターは 4 バイト
    pub fn process(&mut self, input: &[Vec<f32>], frames: usize) -> R<Vec<Vec<f32>>> {
        let ni = self.num_inputs.max(0) as usize;
        let no = self.num_outputs.max(0) as usize;
        let mem = &mut self.emu.mem;
        let in_ptrs = mem.alloc32(ni * 4);
        for c in 0..ni {
            let buf = mem.alloc32(frames * 4);
            let src = input.get(c).map(|v| v.as_slice()).unwrap_or(&[]);
            let mut data = vec![0f32; frames];
            data[..src.len().min(frames)].copy_from_slice(&src[..src.len().min(frames)]);
            mem.write_f32s(buf as u64, &data)?;
            mem.write_n(in_ptrs as u64 + c as u64 * 4, 4, buf as u64)?;
        }
        let out_ptrs = mem.alloc32(no * 4);
        let mut out_bufs = Vec::with_capacity(no);
        for c in 0..no {
            let buf = mem.alloc32(frames * 4);
            mem.write_n(out_ptrs as u64 + c as u64 * 4, 4, buf as u64)?;
            out_bufs.push(buf);
        }
        let process = self.ptr(off::PROCESS_REPLACING)?;
        if process == 0 {
            return Err(Fault("processReplacing が無い".into()));
        }
        self.emu.call32(process, &[Arg::Int(self.effect as u64), Arg::Int(in_ptrs as u64), Arg::Int(out_ptrs as u64), Arg::Int(frames as u64)])?;
        let mut out = Vec::with_capacity(no);
        for buf in out_bufs {
            out.push(self.emu.mem.read_f32s(buf as u64, frames)?);
        }
        Ok(out)
    }

    pub fn name(&mut self) -> R<String> {
        let dispatcher = self.ptr(off::DISPATCHER)?;
        if dispatcher == 0 {
            return Ok(String::new());
        }
        let buf = self.emu.mem.alloc32(64);
        self.emu.call32(dispatcher, &[Arg::Int(self.effect as u64), Arg::Int(EFF_GET_EFFECT_NAME), Arg::Int(0), Arg::Int(0), Arg::Int(buf as u64), Arg::F32(0.0)])?;
        self.emu.mem.read_cstr(buf as u64, 64)
    }
}
