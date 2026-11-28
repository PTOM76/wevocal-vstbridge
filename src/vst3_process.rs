//! VST3 の音の処理（段階 2: IComponent と IAudioProcessor を作り、つまみは既定のまま音を通す）。ホストの側のオブジェクト（IHostApplication など）もここで作る
use crate::emu::{Arg, Emu};
use crate::mem::{Fault, R};
use crate::vst3::{vcall, Vst3};

/// インターフェースの id（VST3 SDK の DECLARE_CLASS_IID の 4 つの数を、Windows の並び（COM と同じ）にしたもの）
pub const fn tuid(l1: u32, l2: u32, l3: u32, l4: u32) -> [u8; 16] {
    [
        l1 as u8,
        (l1 >> 8) as u8,
        (l1 >> 16) as u8,
        (l1 >> 24) as u8,
        (l2 >> 16) as u8,
        (l2 >> 24) as u8,
        l2 as u8,
        (l2 >> 8) as u8,
        (l3 >> 24) as u8,
        (l3 >> 16) as u8,
        (l3 >> 8) as u8,
        l3 as u8,
        (l4 >> 24) as u8,
        (l4 >> 16) as u8,
        (l4 >> 8) as u8,
        l4 as u8,
    ]
}

pub const IID_FUNKNOWN: [u8; 16] = tuid(0x00000000, 0x00000000, 0xC0000000, 0x00000046);
pub const IID_ICOMPONENT: [u8; 16] = tuid(0xE831FF31, 0xF2D54301, 0x928EBBEE, 0x25697802);
pub const IID_IAUDIOPROCESSOR: [u8; 16] = tuid(0x42043F99, 0xB7DA453C, 0xA569E79D, 0x9AAEC33D);
pub const IID_IEDITCONTROLLER: [u8; 16] = tuid(0xDCD7BBE3, 0x7742448D, 0xA874AACC, 0x979C759E);
pub const IID_IHOSTAPPLICATION: [u8; 16] = tuid(0x58E595CC, 0xDB2D4969, 0x8B6AAF8C, 0x36A664E5);

/// 結果の値（Windows の COM と同じ）
pub const K_RESULT_OK: u64 = 0;
pub const K_NO_INTERFACE: u64 = 0x8000_4002;

/// ホストの側のオブジェクトを作る。関数の表は queryInterface、addRef、release のあとに methods。queryInterface は iids（と FUnknown）なら自分を返す
pub fn host_object(emu: &mut Emu, iids: Vec<[u8; 16]>, methods: Vec<crate::emu::HostFn>) -> R<u64> {
    let n = 3 + methods.len();
    let table = emu.mem.alloc(n * 8);
    let obj = emu.mem.alloc(16);
    emu.mem.write_n(obj, 8, table)?;
    let qi = emu.add_thunk(Box::new(move |e: &mut Emu| {
        let (this, iid, out) = (crate::host_stubs::arg(e, 1)?, crate::host_stubs::arg(e, 2)?, crate::host_stubs::arg(e, 3)?);
        let mut id = [0u8; 16];
        e.mem.read(iid, &mut id)?;
        let ok = id == IID_FUNKNOWN || iids.contains(&id);
        if out != 0 {
            e.mem.write_n(out, 8, if ok { this } else { 0 })?;
        }
        Ok(if ok { K_RESULT_OK } else { K_NO_INTERFACE })
    }));
    let add = emu.add_thunk(Box::new(|_e: &mut Emu| Ok(1)));
    let rel = emu.add_thunk(Box::new(|_e: &mut Emu| Ok(1)));
    let mut fns = vec![qi, add, rel];
    for m in methods {
        fns.push(emu.add_thunk(m));
    }
    for (i, f) in fns.iter().enumerate() {
        emu.mem.write_n(table + i as u64 * 8, 8, *f)?;
    }
    Ok(obj)
}

/// UTF-16 の文字列を書く（0 で終える）
pub(crate) fn write_wstr(emu: &mut Emu, at: u64, s: &str, max: usize) -> R<()> {
    let units: Vec<u16> = s.encode_utf16().take(max - 1).chain(std::iter::once(0)).collect();
    for (i, u) in units.iter().enumerate() {
        emu.mem.write_n(at + i as u64 * 2, 2, *u as u64)?;
    }
    Ok(())
}

/// 処理の決まり（ProcessSetup）。32 ビットの浮動小数点、1 回の最大の長さ
const SAMPLE_32: u64 = 0;
pub const BLOCK_MAX: usize = 4096;
/// バスの種類と向き、ステレオ（左と右）
const K_AUDIO: u64 = 0;
const K_INPUT: u64 = 0;
const K_OUTPUT: u64 = 1;
const K_STEREO: u64 = 3;

struct Io {
    frames: usize,
    data: u64,
    in_bufs: Vec<u64>,
    out_bufs: Vec<u64>,
}

/// 音を処理するプラグイン（IComponent と IAudioProcessor）
pub struct Vst3Plugin {
    pub emu: Emu,
    pub component: u64,
    pub processor: u64,
    /// 入力と出力のチャンネルの数（最初のバスだけをステレオで使う。無ければ 0）
    pub inputs: usize,
    pub outputs: usize,
    io: Option<Io>,
}

impl Vst3Plugin {
    /// 音を処理するクラス（category "Audio Module Class"）の最初のものを作り、処理できる状態にする
    pub fn open(dll: &[u8], sample_rate: f64) -> R<Vst3Plugin> {
        let v = Vst3::load(dll)?;
        let class = v.classes.iter().find(|c| c.category == "Audio Module Class").cloned().ok_or_else(|| Fault("音を処理するクラスが無い".into()))?;
        let mut emu = v.emu;
        emu.max_steps = crate::emu::LOAD_MAX_STEPS;
        // ホスト（IHostApplication: getName(String128)、createInstance(cid, iid, obj)）
        let host = host_object(
            &mut emu,
            vec![IID_IHOSTAPPLICATION],
            vec![
                Box::new(|e: &mut Emu| {
                    let at = crate::host_stubs::arg(e, 2)?;
                    write_wstr(e, at, "WeVocal", 128)?;
                    Ok(K_RESULT_OK)
                }),
                // ホストが作るもの（IMessage など）はまだ用意しない
                Box::new(|e: &mut Emu| {
                    let out = crate::host_stubs::arg(e, 4)?;
                    if out != 0 {
                        e.mem.write_n(out, 8, 0)?;
                    }
                    Ok(K_NO_INTERFACE)
                }),
            ],
        )?;
        let cid = emu.mem.alloc(16);
        emu.mem.write(cid, &class.cid)?;
        let iid = emu.mem.alloc(16);
        emu.mem.write(iid, &IID_ICOMPONENT)?;
        let out = emu.mem.alloc(8);
        emu.mem.write_n(out, 8, 0)?;
        // IPluginFactory::createInstance(cid, iid, obj)
        let r = vcall(&mut emu, v.factory, 6, &[Arg::Int(cid), Arg::Int(iid), Arg::Int(out)])?;
        let component = emu.mem.read_n(out, 8)?;
        if r as u32 != 0 || component == 0 {
            return Err(Fault(format!("音を処理するクラスを作れない（{:#x}）", r as u32)));
        }
        // IPluginBase::initialize(context)
        vcall(&mut emu, component, 3, &[Arg::Int(host)])?;
        // IAudioProcessor（queryInterface）
        emu.mem.write(iid, &IID_IAUDIOPROCESSOR)?;
        emu.mem.write_n(out, 8, 0)?;
        vcall(&mut emu, component, 0, &[Arg::Int(iid), Arg::Int(out)])?;
        let processor = emu.mem.read_n(out, 8)?;
        if processor == 0 {
            return Err(Fault("IAudioProcessor が無い".into()));
        }
        // バスの数（IComponent::getBusCount）。最初のバスだけをステレオにして有効にする（ほかはサイドチェーンなど）
        let ins = vcall(&mut emu, component, 7, &[Arg::Int(K_AUDIO), Arg::Int(K_INPUT)])? as u32 as usize;
        let outs = vcall(&mut emu, component, 7, &[Arg::Int(K_AUDIO), Arg::Int(K_OUTPUT)])? as u32 as usize;
        let arr = emu.mem.alloc(8 * (ins + outs).max(1));
        for i in 0..ins + outs {
            emu.mem.write_n(arr + i as u64 * 8, 8, if i == 0 || i == ins { K_STEREO } else { 0 })?;
        }
        // IAudioProcessor::setBusArrangements(inputs, numIns, outputs, numOuts)
        vcall(&mut emu, processor, 3, &[Arg::Int(arr), Arg::Int(ins as u64), Arg::Int(arr + ins as u64 * 8), Arg::Int(outs as u64)])?;
        // IComponent::activateBus(type, dir, index, state)
        if ins > 0 {
            vcall(&mut emu, component, 10, &[Arg::Int(K_AUDIO), Arg::Int(K_INPUT), Arg::Int(0), Arg::Int(1)])?;
        }
        if outs > 0 {
            vcall(&mut emu, component, 10, &[Arg::Int(K_AUDIO), Arg::Int(K_OUTPUT), Arg::Int(0), Arg::Int(1)])?;
        }
        // IAudioProcessor::setupProcessing(ProcessSetup: processMode、symbolicSampleSize、maxSamplesPerBlock、sampleRate)
        let setup = emu.mem.alloc(24);
        emu.mem.write_n(setup, 4, 0)?;
        emu.mem.write_n(setup + 4, 4, SAMPLE_32)?;
        emu.mem.write_n(setup + 8, 4, BLOCK_MAX as u64)?;
        emu.mem.write_n(setup + 16, 8, sample_rate.to_bits())?;
        vcall(&mut emu, processor, 7, &[Arg::Int(setup)])?;
        // IComponent::setActive(true)、IAudioProcessor::setProcessing(true)
        vcall(&mut emu, component, 11, &[Arg::Int(1)])?;
        vcall(&mut emu, processor, 8, &[Arg::Int(1)])?;
        emu.max_steps = crate::emu::MAX_STEPS;
        Ok(Vst3Plugin { emu, component, processor, inputs: if ins > 0 { 2 } else { 0 }, outputs: if outs > 0 { 2 } else { 0 }, io: None })
    }

    /// 音を通す（入力と出力はステレオ。frames は BLOCK_MAX まで）
    pub fn process(&mut self, input: &[Vec<f32>], frames: usize) -> R<Vec<Vec<f32>>> {
        if frames > BLOCK_MAX {
            return Err(Fault("ブロックが長すぎる".into()));
        }
        if self.io.as_ref().map(|io| io.frames) != Some(frames) {
            let (ni, no) = (self.inputs, self.outputs);
            let mem = &mut self.emu.mem;
            // ProcessData（80 バイト）と、入力と出力の AudioBusBuffers（24 バイト: numChannels、silenceFlags、channelBuffers32）
            let data = mem.alloc(80);
            let bus_in = mem.alloc(24);
            let bus_out = mem.alloc(24);
            let ptrs_in = mem.alloc(16);
            let ptrs_out = mem.alloc(16);
            let mut in_bufs = Vec::new();
            let mut out_bufs = Vec::new();
            for c in 0..2u64 {
                let b = mem.alloc(frames * 4);
                mem.write_n(ptrs_in + c * 8, 8, b)?;
                in_bufs.push(b);
                let b = mem.alloc(frames * 4);
                mem.write_n(ptrs_out + c * 8, 8, b)?;
                out_bufs.push(b);
            }
            for (bus, ptrs, n) in [(bus_in, ptrs_in, ni), (bus_out, ptrs_out, no)] {
                mem.write_n(bus, 4, n as u64)?;
                mem.write_n(bus + 8, 8, 0)?;
                mem.write_n(bus + 16, 8, ptrs)?;
            }
            // ProcessData: processMode、symbolicSampleSize、numSamples、numInputs、numOutputs、inputs、outputs（パラメーターとイベントと時刻はまだ無し）
            mem.write(data, &[0u8; 80])?;
            mem.write_n(data + 4, 4, SAMPLE_32)?;
            mem.write_n(data + 8, 4, frames as u64)?;
            mem.write_n(data + 12, 4, (ni > 0) as u64)?;
            mem.write_n(data + 16, 4, (no > 0) as u64)?;
            mem.write_n(data + 24, 8, bus_in)?;
            mem.write_n(data + 32, 8, bus_out)?;
            self.io = Some(Io { frames, data, in_bufs, out_bufs });
        }
        let io = self.io.as_ref().unwrap();
        let (data, in_bufs, out_bufs) = (io.data, io.in_bufs.clone(), io.out_bufs.clone());
        for (c, b) in in_bufs.iter().enumerate() {
            let src = input.get(c).or(input.first()).map(|v| v.as_slice()).unwrap_or(&[]);
            let mut d = vec![0f32; frames];
            let n = src.len().min(frames);
            d[..n].copy_from_slice(&src[..n]);
            self.emu.mem.write_f32s(*b, &d)?;
        }
        // IAudioProcessor::process(data)
        vcall(&mut self.emu, self.processor, 9, &[Arg::Int(data)])?;
        out_bufs.iter().map(|b| self.emu.mem.read_f32s(*b, frames)).collect()
    }
}
