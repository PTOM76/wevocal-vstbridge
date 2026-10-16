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
    pub const PROCESS_REPLACING: u64 = 64;
}

const VST_MAGIC: u64 = 0x5673_7450; // 'VstP'
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
            // cdecl。戻り先を pop した直後なので、esp は 1 番目の引数（effect）を指す。opcode は 2 番目（esp+4）
            let esp = e.cpu.regs[crate::cpu::RSP];
            let opcode = e.mem.read_n(esp + 4, 4)?;
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
        Ok(Vst32 { emu, effect, num_params, num_inputs, num_outputs })
    }

    fn ptr(&self, field: u64) -> R<u32> {
        Ok(self.emu.mem.read_n(self.effect as u64 + field, 4)? as u32)
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
