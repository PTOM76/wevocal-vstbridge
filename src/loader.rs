//! VST2 の DLL の読み込み（DllMain、VSTPluginMain、初期化の dispatch）を、決まった命令数ずつ進める。
//! AudioWorklet の中で読み込むとき、音のスレッドを長く止めないため（読み込みの間は素通し）。一気に読むときも同じ手順を使う
use crate::emu::{Arg, Emu};
use crate::mem::{Fault, R};
use crate::vst::Vst;
use crate::vst32::Vst32;

/// 読み込みの手順の 1 つ
pub(crate) enum Step {
    /// 関数を呼ぶ（TLS のコールバック、DllMain）
    Call(u64, Vec<Arg>),
    /// VSTPluginMain(host)。戻り値が AEffect の番地
    Main(u64, u64),
    /// dispatcher(effect, opcode, index, value, 0, opt)
    Dispatch(u64, u64, u64, f32),
}

/// 読み込んだプラグイン
pub enum Loaded {
    X64(Vst),
    X86(Vst32),
}

/// 途中まで進めた読み込み
pub struct Loader {
    emu: Emu,
    steps: Vec<Step>,
    /// 次に進める手順
    k: usize,
    /// 動かしている途中の呼び出し（戻ったあとに戻す rsp）
    cur: Option<u64>,
    effect: u64,
    /// audioMasterGetTime に返す VstTimeInfo の番地
    time: u64,
    /// DLL の基点
    base: u64,
}

/// PE の Machine（0x14c は 32 ビット、0x8664 は 64 ビット）。PE でなければ None
pub fn machine(dll: &[u8]) -> Option<u16> {
    let pe = u32::from_le_bytes(dll.get(0x3c..0x40)?.try_into().ok()?) as usize;
    if dll.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    Some(u16::from_le_bytes(dll.get(pe + 4..pe + 6)?.try_into().ok()?))
}

impl Loader {
    /// DLL を置き、import をつなぐところまで（ここは命令を動かさないので速い）
    pub fn new(dll: &[u8]) -> R<Loader> {
        let (emu, steps, time, base) = match machine(dll) {
            Some(0x14c) => Vst32::prepare(dll)?,
            Some(0x8664) => Vst::prepare(dll)?,
            Some(m) => return Err(Fault(format!("未対応の CPU です（Machine={m:#x}）"))),
            None => return Err(Fault("DLL（PE）ではありません".into())),
        };
        Ok(Loader { emu, steps, k: 0, cur: None, effect: 0, time, base })
    }

    /// 最後まで読み込む（命令数の上限は LOAD_MAX_STEPS）
    pub fn load(dll: &[u8]) -> R<Loaded> {
        let mut l = Loader::new(dll)?;
        if !l.step(crate::emu::LOAD_MAX_STEPS)? {
            return Err(Fault("読み込みが終わらない（step limit）".into()));
        }
        l.finish()
    }

    /// 命令数 budget まで読み込みを進める。終わったら true
    pub fn step(&mut self, budget: u64) -> R<bool> {
        let start = self.emu.cpu.steps;
        let bits32 = self.emu.bits() == 32;
        while self.k < self.steps.len() {
            let used = self.emu.cpu.steps - start;
            if used >= budget {
                return Ok(false);
            }
            let saved = match self.cur {
                Some(s) => s,
                None => {
                    let (func, args) = match &self.steps[self.k] {
                        Step::Call(f, a) => (*f, a.clone()),
                        Step::Main(f, host) => (*f, vec![Arg::Int(*host)]),
                        &Step::Dispatch(op, index, value, opt) => {
                            // AEffect の dispatcher（64 ビットは 8 バイト目、32 ビットは 4 バイト目）。無ければ飛ばす
                            let (at, w) = if bits32 { (4, 4) } else { (8, 8) };
                            let f = self.emu.mem.read_n(self.effect + at, w)?;
                            if f == 0 {
                                self.k += 1;
                                continue;
                            }
                            (f, vec![Arg::Int(self.effect), Arg::Int(op), Arg::Int(index), Arg::Int(value), Arg::Int(0), Arg::F32(opt)])
                        }
                    };
                    let s = self.emu.begin_call(func, &args)?;
                    self.cur = Some(s);
                    s
                }
            };
            let Some(ret) = self.emu.step_call(budget - used, saved)? else { return Ok(false) };
            self.cur = None;
            if let Step::Main(..) = self.steps[self.k] {
                if ret == 0 {
                    return Err(Fault("VSTPluginMain が 0 を返した".into()));
                }
                let magic = self.emu.mem.read_n(ret, 4)?;
                if magic != 0x5673_7450 {
                    return Err(Fault(format!("magic が違う: {magic:#x}")));
                }
                self.effect = ret;
            }
            self.k += 1;
        }
        Ok(true)
    }

    /// 読み込みが終わったものをプラグインにする
    pub fn finish(self) -> R<Loaded> {
        if self.k < self.steps.len() {
            return Err(Fault("読み込みが終わっていない".into()));
        }
        Ok(if self.emu.bits() == 32 { Loaded::X86(Vst32::from_loaded(self.emu, self.effect, self.time, self.base)?) } else { Loaded::X64(Vst::from_loaded(self.emu, self.effect, self.time)?) })
    }
}
