//! 32 ビットの VST2 の DLL を読み込み、エミュレーターの上で動かす。64 ビット版（vst.rs）と違い、
//! ポインターが 4 バイトなので AEffect の中の位置（オフセット）が変わる。import は host_stubs.rs のスタブで解決する
use crate::emu::{Arg, Emu};
use crate::loader::Step;
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
const EFF_PROCESS_EVENTS: u64 = 25;
const EFF_GET_PARAM_NAME: u64 = 8;
const EFF_GET_EFFECT_NAME: u64 = 45;

pub struct Vst32 {
    pub emu: Emu,
    pub effect: u32,
    pub num_params: i32,
    pub num_inputs: i32,
    pub num_outputs: i32,
    /// DLL を読み込んだ番地（リソースを探すのに使う）
    pub base: u32,
    /// 音を通すときのバッファ（ブロックごとに確保し直さない。メモリが増え続けるため）
    io: Option<Io>,
    /// audioMasterGetTime に返す VstTimeInfo の番地（process のたびに位置を進める）
    time: u64,
    /// effProcessEvents に渡す VstEvents と VstMidiEvent 1 つ（最初に送るときに確保する）
    events: u64,
}

/// processReplacing に渡すバッファの番地
struct Io {
    frames: usize,
    in_ptrs: u32,
    in_bufs: Vec<u32>,
    out_ptrs: u32,
    out_bufs: Vec<u32>,
}

impl Vst32 {
    /// DLL のバイト列から読み込む。import はログに出すだけで、知っているものだけ動く（host_stubs.rs）。手順は loader.rs で最後まで進める
    pub fn load(dll: &[u8]) -> R<Vst32> {
        match crate::loader::Loader::load(dll)? {
            crate::loader::Loaded::X86(v) => Ok(v),
            crate::loader::Loaded::X64(_) => Err(Fault("64 ビットの DLL です".into())),
        }
    }

    /// 読み込みの前半: DLL を置いて import をつなぎ、動かす手順（DllMain、VSTPluginMain、初期化の dispatch）を返す
    pub(crate) fn prepare(dll: &[u8]) -> R<(Emu, Vec<Step>, u64, u64)> {
        let mut emu = Emu::new32();
        let img = pe32::load(&mut emu, dll, host_stubs::resolve, host_stubs::unknown)?;
        emu.gui.base = img.base as u64;
        let entry = img.export("VSTPluginMain").or_else(|| img.export("main")).ok_or_else(|| Fault("VSTPluginMain が無い".into()))?;

        // ホストへの呼び出し（audioMasterCallback）。opcode は 2 番目の引数（スタックの上から 2 つ目）
        let time = crate::timeinfo::alloc(&mut emu.mem)?;
        let host = emu.add_thunk(Box::new(move |e: &mut Emu| {
            // audioMasterCallback(effect, opcode, index, value, ptr, opt) は cdecl。戻り先を pop した直後なので esp は effect を指す
            let esp = e.cpu.regs[crate::cpu::RSP];
            let opcode = e.mem.read_n(esp + 4, 4)?;
            if std::env::var("VSTBRIDGE_TRACE").is_ok() {
                eprintln!("[audioMaster] opcode={opcode}");
            }
            // 0=automate（画面でつまみを動かした。index と opt の値を記録して、ホストが音の側へ渡す）、1=version（2400）、7=getTime（VstTimeInfo）、ほかは 0
            if opcode == 0 {
                let index = e.mem.read_n(esp + 8, 4)? as i32;
                let value = f32::from_bits(e.mem.read_n(esp + 20, 4)? as u32);
                e.gui.automation.push((index, value));
            }
            Ok(match opcode {
                1 => 2400,
                7 => time,
                _ => 0,
            })
        }));

        let mut steps = Vec::new();
        if img.entry != img.base {
            steps.push(Step::Call(img.entry as u64, vec![Arg::Int(img.base as u64), Arg::Int(1), Arg::Int(0)]));
        }
        steps.push(Step::Main(entry as u64, host));
        // ホストが普通に行う初期化（開く、プログラム、サンプルレート、ブロックサイズ、電源オン）。これをしないと内部バッファが未初期化のまま
        steps.push(Step::Dispatch(EFF_OPEN, 0, 0, 0.0));
        steps.push(Step::Dispatch(EFF_SET_PROGRAM, 0, 0, 0.0));
        steps.push(Step::Dispatch(EFF_SET_SAMPLE_RATE, 0, 0, 48000.0));
        // ブロックサイズは value で渡す（ptr ではない）
        steps.push(Step::Dispatch(EFF_SET_BLOCK_SIZE, 0, 2048, 0.0));
        steps.push(Step::Dispatch(EFF_MAINS_CHANGED, 0, 1, 0.0));
        Ok((emu, steps, time, img.base as u64))
    }

    /// 読み込みが終わった Emu と AEffect の番地からプラグインにする
    pub(crate) fn from_loaded(emu: Emu, effect: u64, time: u64, base: u64) -> R<Vst32> {
        let effect = effect as u32;
        let num_params = emu.mem.read_n(effect as u64 + off::NUM_PARAMS, 4)? as i32;
        let num_inputs = emu.mem.read_n(effect as u64 + off::NUM_INPUTS, 4)? as i32;
        let num_outputs = emu.mem.read_n(effect as u64 + off::NUM_OUTPUTS, 4)? as i32;
        Ok(Vst32 { emu, effect, num_params, num_inputs, num_outputs, base: base as u32, io: None, events: 0, time })
    }

    /// dispatcher(effect, opcode, index, value, ptr, opt) を呼ぶ
    pub fn dispatch(&mut self, opcode: u64, index: u64, value: u64, ptr: u64, opt: f32) -> R<u64> {
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

    /// MIDI のメッセージ（3 バイト）を 1 つ送る（effProcessEvents）。delta はブロックの頭からのサンプル数
    pub fn send_midi(&mut self, data: [u8; 3], delta: i32) -> R<()> {
        // VstEvents（numEvents、reserved、events[2]）のあとに VstMidiEvent（32 バイト）を置く
        let head = 12;
        if self.events == 0 {
            self.events = self.emu.mem.alloc32(12 + 32) as u64;
        }
        let (ev, mid) = (self.events, self.events + head);
        let mem = &mut self.emu.mem;
        mem.write_n(ev, 4, 1)?;
        mem.write_n(ev + 4, 4, 0)?;
        mem.write_n(ev + 8, 4, mid)?;
        let mut m = [0u8; 32];
        m[0..4].copy_from_slice(&1i32.to_le_bytes());
        m[4..8].copy_from_slice(&32i32.to_le_bytes());
        m[8..12].copy_from_slice(&delta.to_le_bytes());
        m[24..27].copy_from_slice(&data);
        mem.write(mid, &m)?;
        self.dispatch(EFF_PROCESS_EVENTS, 0, 0, ev, 0.0)?;
        Ok(())
    }

    /// 音を通す（processReplacing(effect, in**, out**, n)）。ポインターは 4 バイト
    pub fn process(&mut self, input: &[Vec<f32>], frames: usize) -> R<Vec<Vec<f32>>> {
        let ni = self.num_inputs.max(0) as usize;
        let no = self.num_outputs.max(0) as usize;
        // バッファは、ブロックの長さが変わったときだけ確保し直す
        if self.io.as_ref().map(|io| io.frames) != Some(frames) {
            let mem = &mut self.emu.mem;
            let in_ptrs = mem.alloc32(ni * 4);
            let mut in_bufs = Vec::with_capacity(ni);
            for c in 0..ni {
                let buf = mem.alloc32(frames * 4);
                mem.write_n(in_ptrs as u64 + c as u64 * 4, 4, buf as u64)?;
                in_bufs.push(buf);
            }
            let out_ptrs = mem.alloc32(no * 4);
            let mut out_bufs = Vec::with_capacity(no);
            for c in 0..no {
                let buf = mem.alloc32(frames * 4);
                mem.write_n(out_ptrs as u64 + c as u64 * 4, 4, buf as u64)?;
                out_bufs.push(buf);
            }
            self.io = Some(Io { frames, in_ptrs, in_bufs, out_ptrs, out_bufs });
        }
        let io = self.io.as_ref().unwrap();
        for (c, buf) in io.in_bufs.iter().enumerate() {
            let src = input.get(c).map(|v| v.as_slice()).unwrap_or(&[]);
            let mut data = vec![0f32; frames];
            data[..src.len().min(frames)].copy_from_slice(&src[..src.len().min(frames)]);
            self.emu.mem.write_f32s(*buf as u64, &data)?;
        }
        let (in_ptrs, out_ptrs, out_bufs) = (io.in_ptrs, io.out_ptrs, io.out_bufs.clone());
        let process = self.ptr(off::PROCESS_REPLACING)?;
        if process == 0 {
            return Err(Fault("processReplacing が無い".into()));
        }
        self.emu.call32(process, &[Arg::Int(self.effect as u64), Arg::Int(in_ptrs as u64), Arg::Int(out_ptrs as u64), Arg::Int(frames as u64)])?;
        crate::timeinfo::advance(&mut self.emu.mem, self.time, frames)?;
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
