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
pub const IID_IPARAMETERCHANGES: [u8; 16] = tuid(0xA4779663, 0x0BB64A56, 0xB44384A8, 0x466FEB9D);
pub const IID_ICOMPONENTHANDLER: [u8; 16] = tuid(0x93A0BEA3, 0x0BD045DB, 0x8E890B0C, 0xC1E46AC6);
pub const IID_IPLUGVIEW: [u8; 16] = tuid(0x5BC32507, 0xD06049EA, 0xA6151B52, 0x2B755B29);
pub const IID_IPARAMVALUEQUEUE: [u8; 16] = tuid(0x01263A18, 0xED074F6F, 0x98C9D356, 0x4686F9BA);

/// つまみの情報（ParameterInfo: id、title、shortTitle、units（UTF-16 の 128 文字ずつ）、stepCount、defaultNormalizedValue、unitId、flags）
#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub id: u32,
    pub title: String,
    pub units: String,
    pub steps: i32,
    pub default: f64,
    pub flags: i32,
}

/// つまみのフラグ（kIsReadOnly、kIsHidden、kIsBypass）
pub const PARAM_READ_ONLY: i32 = 1 << 1;
pub const PARAM_HIDDEN: i32 = 1 << 4;
pub const PARAM_BYPASS: i32 = 1 << 16;

/// 次の process で渡す、つまみの値の変化（id と 0〜1 の値）
type Pending = std::rc::Rc<std::cell::RefCell<Vec<(u32, f64)>>>;
/// 1 回の process で渡せるつまみの数
const QUEUES: usize = 256;

fn read_wstr(emu: &Emu, at: u64, max: usize) -> R<String> {
    let mut units = Vec::new();
    for i in 0..max {
        let u = emu.mem.read_n(at + i as u64 * 2, 2)? as u16;
        if u == 0 {
            break;
        }
        units.push(u);
    }
    Ok(String::from_utf16_lossy(&units))
}

/// 結果の値（Windows の COM と同じ）
pub const K_RESULT_OK: u64 = 0;
pub const K_NO_INTERFACE: u64 = 0x8000_4002;

/// ホストの側のオブジェクトを作る。関数の表は queryInterface、addRef、release のあとに methods。queryInterface は iids（と FUnknown）なら自分を返す
pub fn host_object(emu: &mut Emu, iids: Vec<[u8; 16]>, methods: Vec<crate::emu::HostFn>) -> R<u64> {
    let table = host_table(emu, iids, methods)?;
    host_instance(emu, table, 0)
}

/// 同じ関数の表を持つオブジェクトを作る（16 バイト: 関数の表、その後ろに自由に使える値 1 つ）
pub fn host_instance(emu: &mut Emu, table: u64, value: u64) -> R<u64> {
    let obj = emu.mem.alloc(16);
    emu.mem.write_n(obj, 8, table)?;
    emu.mem.write_n(obj + 8, 8, value)?;
    Ok(obj)
}

/// ホストの側のオブジェクトの関数の表を作る（host_instance で何個でもオブジェクトにできる）
pub fn host_table(emu: &mut Emu, iids: Vec<[u8; 16]>, methods: Vec<crate::emu::HostFn>) -> R<u64> {
    let n = 3 + methods.len();
    let table = emu.mem.alloc(n * 8);
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
    Ok(table)
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
    /// IEditController（別のクラスか、component と同じオブジェクト。無ければ 0）
    pub controller: u64,
    pub params: Vec<ParamInfo>,
    /// クラスの名前と cid
    pub name: String,
    pub cid: [u8; 16],
    /// コントローラーが別のオブジェクトか（状態を両方に伝える）
    separate: bool,
    streams: crate::vst3_host::Streams,
    pending: Pending,
    /// IParameterChanges（process に渡す）
    changes: u64,
    /// 入力と出力のチャンネルの数（最初のバスだけをステレオで使う。無ければ 0）
    pub inputs: usize,
    pub outputs: usize,
    io: Option<Io>,
    /// 画面（IPlugView。開いていなければ 0）と、その親のウィンドウ
    pub(crate) view: u64,
    pub(crate) view_root: std::rc::Rc<std::cell::Cell<u32>>,
}

impl Vst3Plugin {
    /// 音を処理するクラス（category "Audio Module Class"）の最初のものを作り、処理できる状態にする
    pub fn open(dll: &[u8], sample_rate: f64) -> R<Vst3Plugin> {
        let v = Vst3::load(dll)?;
        let class = v.classes.iter().find(|c| c.category == "Audio Module Class").cloned().ok_or_else(|| Fault("音を処理するクラスが無い".into()))?;
        let mut emu = v.emu;
        emu.max_steps = crate::emu::LOAD_MAX_STEPS;
        // ホスト（IHostApplication: getName(String128)、createInstance(cid, iid, obj)）
        let make = crate::vst3_host::message_factory(&mut emu)?;
        let host = host_object(
            &mut emu,
            vec![IID_IHOSTAPPLICATION],
            vec![
                Box::new(|e: &mut Emu| {
                    let at = crate::host_stubs::arg(e, 2)?;
                    write_wstr(e, at, "WeVocal", 128)?;
                    Ok(K_RESULT_OK)
                }),
                // ホストが作るもの（IMessage、IAttributeList。音の処理とコントローラーのやり取りに使う）
                Box::new(move |e: &mut Emu| crate::vst3_host::create_instance(e, &make)),
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
        // コントローラー（IComponent::getControllerClassId で別のクラスなら作る。無ければ component が兼ねているか queryInterface で見る）
        let ctrl_cid = emu.mem.alloc(16);
        emu.mem.write(ctrl_cid, &[0u8; 16])?;
        vcall(&mut emu, component, 5, &[Arg::Int(ctrl_cid)])?;
        let mut cc = [0u8; 16];
        emu.mem.read(ctrl_cid, &mut cc)?;
        emu.mem.write(iid, &IID_IEDITCONTROLLER)?;
        emu.mem.write_n(out, 8, 0)?;
        let controller = if cc != [0u8; 16] && cc != class.cid {
            let r = vcall(&mut emu, v.factory, 6, &[Arg::Int(ctrl_cid), Arg::Int(iid), Arg::Int(out)])?;
            let c = emu.mem.read_n(out, 8)?;
            if r as u32 == 0 && c != 0 {
                vcall(&mut emu, c, 3, &[Arg::Int(host)])?;
                // 音の処理とコントローラーをつなぐ（IConnectionPoint::connect を両方から。JUCE などはここで互いを知る）
                crate::vst3_host::connect(&mut emu, component, c)?;
            }
            c
        } else {
            vcall(&mut emu, component, 0, &[Arg::Int(iid), Arg::Int(out)])?;
            emu.mem.read_n(out, 8)?
        };
        // コントローラーが別のオブジェクトなら、音の処理の状態を伝える（IEditController::setComponentState。ホストの決まった手順）
        let separate = controller != 0 && controller != component;
        let streams = crate::vst3_host::Streams::new(&mut emu)?;
        if separate {
            let s = streams.open(&mut emu, &[])?;
            vcall(&mut emu, component, 13, &[Arg::Int(s)])?;
            let data = streams.take(s);
            let s = streams.open(&mut emu, &data)?;
            vcall(&mut emu, controller, 5, &[Arg::Int(s)])?;
            streams.take(s);
        }
        // つまみの一覧（IEditController::getParameterCount、getParameterInfo）
        let mut params = Vec::new();
        if controller != 0 {
            let n = vcall(&mut emu, controller, 8, &[])? as u32 as i32;
            let info = emu.mem.alloc(800);
            for i in 0..n.max(0) {
                if vcall(&mut emu, controller, 9, &[Arg::Int(i as u64), Arg::Int(info)])? as u32 != 0 {
                    continue;
                }
                params.push(ParamInfo {
                    id: emu.mem.read_n(info, 4)? as u32,
                    title: read_wstr(&emu, info + 4, 128)?,
                    units: read_wstr(&emu, info + 516, 128)?,
                    steps: emu.mem.read_n(info + 772, 4)? as u32 as i32,
                    default: f64::from_bits(emu.mem.read_n(info + 776, 8)?),
                    flags: emu.mem.read_n(info + 788, 4)? as u32 as i32,
                });
            }
        }
        // 画面でつまみを動かしたときの知らせ（IComponentHandler: beginEdit、performEdit、endEdit、restartComponent）。
        // performEdit(id, 値) は、つまみの番号に直して gui.automation に積む（VST2 の audioMasterAutomate と同じ。JS が取り出して処理の側へ渡す）
        if controller != 0 {
            let ids: Vec<u32> = params.iter().map(|q| q.id).collect();
            let handler = host_object(
                &mut emu,
                vec![IID_ICOMPONENTHANDLER],
                vec![
                    Box::new(|_e: &mut Emu| Ok(K_RESULT_OK)),
                    Box::new(move |e: &mut Emu| {
                        let id = crate::host_stubs::arg(e, 2)? as u32;
                        // 値（double）は 3 番目の引数なので xmm2
                        let v = f64::from_bits(e.cpu.xmm[2] as u64);
                        if let Some(i) = ids.iter().position(|&x| x == id) {
                            e.gui.automation.push((i as i32, v as f32));
                        }
                        Ok(K_RESULT_OK)
                    }),
                    Box::new(|_e: &mut Emu| Ok(K_RESULT_OK)),
                    Box::new(|_e: &mut Emu| Ok(K_RESULT_OK)),
                ],
            )?;
            // IEditController::setComponentHandler(handler)
            vcall(&mut emu, controller, 16, &[Arg::Int(handler)])?;
        }
        // つまみの変化を process に渡す IParameterChanges と、つまみごとの IParamValueQueue（オブジェクトの後ろの値が pending の何番目か）
        let pending: Pending = Default::default();
        let (p1, p2) = (pending.clone(), pending.clone());
        let queue_table = host_table(
            &mut emu,
            vec![IID_IPARAMVALUEQUEUE],
            vec![
                // getParameterId()
                Box::new(move |e: &mut Emu| {
                    let k = e.mem.read_n(crate::host_stubs::arg(e, 1)? + 8, 8)? as usize;
                    Ok(p1.borrow().get(k).map(|x| x.0).unwrap_or(0) as u64)
                }),
                // getPointCount()
                Box::new(|_e: &mut Emu| Ok(1)),
                // getPoint(index, sampleOffset, value)。ブロックの頭で 1 つ
                Box::new(move |e: &mut Emu| {
                    let k = e.mem.read_n(crate::host_stubs::arg(e, 1)? + 8, 8)? as usize;
                    let (off, val) = (crate::host_stubs::arg(e, 3)?, crate::host_stubs::arg(e, 4)?);
                    let v = p2.borrow().get(k).map(|x| x.1).unwrap_or(0.0);
                    e.mem.write_n(off, 4, 0)?;
                    e.mem.write_n(val, 8, v.to_bits())?;
                    Ok(K_RESULT_OK)
                }),
                // addPoint（プラグインからは足さない）
                Box::new(|_e: &mut Emu| Ok(1)),
            ],
        )?;
        let queues: Vec<u64> = (0..QUEUES).map(|k| host_instance(&mut emu, queue_table, k as u64)).collect::<R<_>>()?;
        let p3 = pending.clone();
        let changes = host_object(
            &mut emu,
            vec![IID_IPARAMETERCHANGES],
            vec![
                // getParameterCount()
                Box::new(move |_e: &mut Emu| Ok(p3.borrow().len().min(QUEUES) as u64)),
                // getParameterData(index)
                Box::new(move |e: &mut Emu| {
                    let k = crate::host_stubs::arg(e, 2)? as u32 as usize;
                    Ok(queues.get(k).copied().unwrap_or(0))
                }),
                // addParameterData（プラグインからは足さない）
                Box::new(|_e: &mut Emu| Ok(0)),
            ],
        )?;
        // IComponent::setActive(true)、IAudioProcessor::setProcessing(true)
        vcall(&mut emu, component, 11, &[Arg::Int(1)])?;
        vcall(&mut emu, processor, 8, &[Arg::Int(1)])?;
        emu.max_steps = crate::emu::MAX_STEPS;
        Ok(Vst3Plugin { name: class.name.clone(), cid: class.cid, emu, component, processor, controller, params, separate, streams, pending, changes, inputs: if ins > 0 { 2 } else { 0 }, outputs: if outs > 0 { 2 } else { 0 }, io: None, view: 0, view_root: Default::default() })
    }

    /// つまみ index（params の順）を 0〜1 の値にする。次の process で処理に渡し、コントローラーにも知らせる
    /// cid の文字列（32 桁の 16 進。VST SDK の FUID::toString と同じく、COM の並びの先頭 3 つは数として読む。.vstpreset と DAWproject で使う）
    pub fn cid_string(&self) -> String {
        let c = &self.cid;
        let mut s = format!("{:08X}{:04X}{:04X}", u32::from_le_bytes([c[0], c[1], c[2], c[3]]), u16::from_le_bytes([c[4], c[5]]), u16::from_le_bytes([c[6], c[7]]));
        for b in &c[8..] {
            s.push_str(&format!("{b:02X}"));
        }
        s
    }

    /// 識別番号（cid の先頭 4 バイト。VST2 の uniqueID の代わり）
    pub fn unique_id(&self) -> u32 {
        u32::from_le_bytes([self.cid[0], self.cid[1], self.cid[2], self.cid[3]])
    }

    pub fn set_param(&mut self, index: usize, value: f64) -> R<()> {
        let Some(p) = self.params.get(index) else { return Ok(()) };
        let id = p.id;
        {
            let mut q = self.pending.borrow_mut();
            q.retain(|x| x.0 != id);
            q.push((id, value));
        }
        if self.controller != 0 {
            // IEditController::setParamNormalized(id, value)
            vcall(&mut self.emu, self.controller, 15, &[Arg::Int(id as u64), Arg::F64(value)])?;
        }
        Ok(())
    }

    /// 状態を取る（音の処理の状態と、別のコントローラーの状態。長さ 4 バイト（リトルエンディアン）と中身を 2 つ並べる）
    pub fn get_state(&mut self) -> R<Vec<u8>> {
        // 渡していないつまみの変化は、無音を短く通して音の処理に届けてから保存する
        if !self.pending.borrow().is_empty() {
            let z = vec![0f32; 32];
            let input: Vec<Vec<f32>> = (0..self.inputs.max(1)).map(|_| z.clone()).collect();
            self.process(&input, 32)?;
        }
        let s = self.streams.open(&mut self.emu, &[])?;
        // IComponent::getState(stream)
        vcall(&mut self.emu, self.component, 13, &[Arg::Int(s)])?;
        let comp = self.streams.take(s);
        let mut ctrl = Vec::new();
        if self.separate {
            let s = self.streams.open(&mut self.emu, &[])?;
            // IEditController::getState(stream)
            vcall(&mut self.emu, self.controller, 7, &[Arg::Int(s)])?;
            ctrl = self.streams.take(s);
        }
        let mut out = Vec::with_capacity(8 + comp.len() + ctrl.len());
        for part in [&comp, &ctrl] {
            out.extend_from_slice(&(part.len() as u32).to_le_bytes());
            out.extend_from_slice(part);
        }
        Ok(out)
    }

    /// 状態を戻す（get_state の形）。音の処理に戻し、別のコントローラーにも伝える。つまみの値は読み返す（get_param）
    pub fn set_state(&mut self, data: &[u8]) -> R<()> {
        let part = |at: usize| -> Option<(&[u8], usize)> {
            let n = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
            Some((data.get(at + 4..at + 4 + n)?, at + 4 + n))
        };
        let Some((comp, next)) = part(0) else { return Err(Fault("状態の形が違う".into())) };
        let ctrl = part(next).map(|x| x.0).unwrap_or(&[]);
        // IComponent::setState(stream)
        let s = self.streams.open(&mut self.emu, comp)?;
        vcall(&mut self.emu, self.component, 12, &[Arg::Int(s)])?;
        self.streams.take(s);
        if self.separate {
            // IEditController::setComponentState(stream)、setState(stream)
            let s = self.streams.open(&mut self.emu, comp)?;
            vcall(&mut self.emu, self.controller, 5, &[Arg::Int(s)])?;
            self.streams.take(s);
            if !ctrl.is_empty() {
                let s = self.streams.open(&mut self.emu, ctrl)?;
                vcall(&mut self.emu, self.controller, 6, &[Arg::Int(s)])?;
                self.streams.take(s);
            }
        }
        Ok(())
    }

    /// つまみ index の今の値（0〜1。IEditController::getParamNormalized）
    pub fn get_param(&mut self, index: usize) -> R<f64> {
        let Some(p) = self.params.get(index) else { return Ok(0.0) };
        if self.controller == 0 {
            return Ok(p.default);
        }
        vcall(&mut self.emu, self.controller, 14, &[Arg::Int(p.id as u64)])?;
        Ok(f64::from_bits(self.emu.cpu.xmm[0] as u64))
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
        // 変えたつまみがあれば、inputParameterChanges で渡す（渡したら空にする）
        let any = !self.pending.borrow().is_empty();
        self.emu.mem.write_n(data + 40, 8, if any { self.changes } else { 0 })?;
        // IAudioProcessor::process(data)
        let r = vcall(&mut self.emu, self.processor, 9, &[Arg::Int(data)]);
        self.pending.borrow_mut().clear();
        r?;
        out_bufs.iter().map(|b| self.emu.mem.read_f32s(*b, frames)).collect()
    }
}
