//! VST3 のホストの側のオブジェクトのうち、メッセージ（IMessage、IAttributeList）と、音の処理とコントローラーをつなぐ処理（IConnectionPoint）。
//! メッセージは IHostApplication::createInstance で作り、値はホスト（Rust）の側に持つ。バイト列はエミュレーターのメモリに写して、その番地を返す
use crate::emu::{Arg, Emu};
use crate::host_stubs::arg;
use crate::mem::R;
use crate::vst3::vcall;
use crate::vst3_process::{host_instance, host_table, tuid, K_NO_INTERFACE, K_RESULT_OK};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub const IID_ICONNECTIONPOINT: [u8; 16] = tuid(0x70A4156F, 0x6E6E4026, 0x989148BF, 0xAA60D8D1);
pub const IID_IMESSAGE: [u8; 16] = tuid(0x936F033B, 0xC6C047DB, 0xBB0882F8, 0x13C1E613);
pub const IID_IATTRIBUTELIST: [u8; 16] = tuid(0x1E5F0AEB, 0xCC7F4533, 0xA2544011, 0x38AD5EE4);

/// 結果: 見つからない（kResultFalse）
const K_RESULT_FALSE: u64 = 1;

#[derive(Clone)]
enum Value {
    Int(u64),
    Float(f64),
    /// UTF-16（0 終わりは含まない）
    Str(Vec<u16>),
    /// エミュレーターのメモリに写したバイト列（番地と長さ）
    Bin(u64, u32),
}

/// メッセージの値の入れ物（オブジェクトの番地 → 名前 → 値）と、メッセージの名前（番地 → 0 終わりの文字列の番地）
#[derive(Default)]
struct Store {
    attrs: HashMap<u64, HashMap<String, Value>>,
    ids: HashMap<u64, u64>,
}

/// メッセージを作る関数（IHostApplication::createInstance から呼ぶ）。作ったメッセージ（IMessage）の番地を返す
pub type NewMessage = Rc<dyn Fn(&mut Emu) -> R<u64>>;

fn read_key(e: &Emu, at: u64) -> R<String> {
    e.mem.read_cstr(at, 128)
}

/// メッセージと値の入れ物の関数の表を作り、メッセージを 1 つ作る関数を返す
pub fn message_factory(emu: &mut Emu) -> R<NewMessage> {
    let store: Rc<RefCell<Store>> = Default::default();
    let attr_table = {
        let s = store.clone();
        let set = move |v: fn(&mut Emu) -> R<Value>| {
            let s = s.clone();
            Box::new(move |e: &mut Emu| -> R<u64> {
                let (this, key) = (arg(e, 1)?, read_key(e, arg(e, 2)?)?);
                let val = v(e)?;
                s.borrow_mut().attrs.entry(this).or_default().insert(key, val);
                Ok(K_RESULT_OK)
            }) as crate::emu::HostFn
        };
        let get = |s: Rc<RefCell<Store>>, f: fn(&mut Emu, Value) -> R<bool>| {
            Box::new(move |e: &mut Emu| -> R<u64> {
                let (this, key) = (arg(e, 1)?, read_key(e, arg(e, 2)?)?);
                let v = s.borrow().attrs.get(&this).and_then(|m| m.get(&key)).cloned();
                Ok(match v {
                    Some(v) => {
                        if f(e, v)? {
                            K_RESULT_OK
                        } else {
                            K_RESULT_FALSE
                        }
                    }
                    None => K_RESULT_FALSE,
                })
            }) as crate::emu::HostFn
        };
        host_table(
            emu,
            vec![IID_IATTRIBUTELIST],
            vec![
                // setInt(id, int64)、getInt(id, int64*)
                set(|e| Ok(Value::Int(arg(e, 3)?))),
                get(store.clone(), |e, v| match v {
                    Value::Int(x) => e.mem.write_n(arg(e, 3)?, 8, x).map(|_| true),
                    _ => Ok(false),
                }),
                // setFloat(id, double)（double は xmm2）、getFloat(id, double*)
                set(|e| Ok(Value::Float(f64::from_bits(e.cpu.xmm[2] as u64)))),
                get(store.clone(), |e, v| match v {
                    Value::Float(x) => e.mem.write_n(arg(e, 3)?, 8, x.to_bits()).map(|_| true),
                    _ => Ok(false),
                }),
                // setString(id, TChar*)、getString(id, TChar*, sizeInBytes)
                set(|e| {
                    let at = arg(e, 3)?;
                    let mut u = Vec::new();
                    for i in 0..4096u64 {
                        let c = e.mem.read_n(at + i * 2, 2)? as u16;
                        if c == 0 {
                            break;
                        }
                        u.push(c);
                    }
                    Ok(Value::Str(u))
                }),
                get(store.clone(), |e, v| match v {
                    Value::Str(u) => {
                        let (at, size) = (arg(e, 3)?, arg(e, 4)? as u32 as usize / 2);
                        if size == 0 {
                            return Ok(false);
                        }
                        let n = u.len().min(size - 1);
                        for (i, c) in u[..n].iter().chain(std::iter::once(&0)).enumerate() {
                            e.mem.write_n(at + i as u64 * 2, 2, *c as u64)?;
                        }
                        Ok(true)
                    }
                    _ => Ok(false),
                }),
                // setBinary(id, data, size)。中身はエミュレーターのメモリに写して持つ
                set(|e| {
                    let (data, size) = (arg(e, 3)?, arg(e, 4)? as u32);
                    let mut buf = vec![0u8; size as usize];
                    e.mem.read(data, &mut buf)?;
                    let at = e.mem.alloc(size.max(1) as usize);
                    e.mem.write(at, &buf)?;
                    Ok(Value::Bin(at, size))
                }),
                // getBinary(id, const void** data, uint32* size)
                get(store.clone(), |e, v| match v {
                    Value::Bin(at, size) => {
                        e.mem.write_n(arg(e, 3)?, 8, at)?;
                        e.mem.write_n(arg(e, 4)?, 4, size as u64)?;
                        Ok(true)
                    }
                    _ => Ok(false),
                }),
            ],
        )?
    };
    let (s1, s2) = (store.clone(), store.clone());
    let msg_table = host_table(
        emu,
        vec![IID_IMESSAGE],
        vec![
            // getMessageID() → 0 終わりの文字列の番地
            Box::new(move |e: &mut Emu| Ok(s1.borrow().ids.get(&arg(e, 1)?).copied().unwrap_or(0))),
            // setMessageID(const char*)。写して持つ
            Box::new(move |e: &mut Emu| {
                let (this, at) = (arg(e, 1)?, arg(e, 2)?);
                let id = e.mem.read_cstr(at, 256)?;
                let copy = e.mem.alloc(id.len() + 1);
                e.mem.write(copy, id.as_bytes())?;
                e.mem.write_n(copy + id.len() as u64, 1, 0)?;
                s2.borrow_mut().ids.insert(this, copy);
                Ok(0)
            }),
            // getAttributes() → メッセージの後ろの値に置いた IAttributeList
            Box::new(|e: &mut Emu| e.mem.read_n(arg(e, 1)? + 8, 8)),
        ],
    )?;
    Ok(Rc::new(move |e: &mut Emu| {
        let attrs = host_instance(e, attr_table, 0)?;
        host_instance(e, msg_table, attrs)
    }))
}

/// IHostApplication::createInstance(cid, iid, obj)。IMessage と IAttributeList を作る。ほかは作れない
pub fn create_instance(e: &mut Emu, make: &NewMessage) -> R<u64> {
    let (iid, out) = (arg(e, 3)?, arg(e, 4)?);
    let mut id = [0u8; 16];
    e.mem.read(iid, &mut id)?;
    let obj = if id == IID_IMESSAGE {
        make(e)?
    } else if id == IID_IATTRIBUTELIST {
        let m = make(e)?;
        e.mem.read_n(m + 8, 8)?
    } else {
        0
    };
    if out != 0 {
        e.mem.write_n(out, 8, obj)?;
    }
    Ok(if obj != 0 { K_RESULT_OK } else { K_NO_INTERFACE })
}

pub const IID_IBSTREAM: [u8; 16] = tuid(0xC3BF6EA2, 0x30994752, 0x9B6BF990, 0x1EE33E9B);

/// メモリのストリーム（IBStream）。中身はホストの側に持ち、オブジェクトの番地で引く
#[derive(Default)]
struct Stream {
    data: Vec<u8>,
    pos: usize,
}

/// ストリームの関数の表と中身。new で空か、中身のあるストリームを作り、take で中身を取り出す
pub struct Streams {
    table: u64,
    all: Rc<RefCell<HashMap<u64, Stream>>>,
}

impl Streams {
    pub fn new(emu: &mut Emu) -> R<Streams> {
        let all: Rc<RefCell<HashMap<u64, Stream>>> = Default::default();
        let (a, b, c, d) = (all.clone(), all.clone(), all.clone(), all.clone());
        let table = host_table(
            emu,
            vec![IID_IBSTREAM],
            vec![
                // read(buffer, numBytes, numBytesRead*)
                Box::new(move |e: &mut Emu| {
                    let (this, buf, n, done) = (arg(e, 1)?, arg(e, 2)?, arg(e, 3)? as u32 as usize, arg(e, 4)?);
                    let chunk = {
                        let mut m = a.borrow_mut();
                        let s = m.entry(this).or_default();
                        let end = (s.pos + n).min(s.data.len());
                        let chunk = s.data[s.pos.min(end)..end].to_vec();
                        s.pos = end;
                        chunk
                    };
                    e.mem.write(buf, &chunk)?;
                    if done != 0 {
                        e.mem.write_n(done, 4, chunk.len() as u64)?;
                    }
                    Ok(K_RESULT_OK)
                }),
                // write(buffer, numBytes, numBytesWritten*)
                Box::new(move |e: &mut Emu| {
                    let (this, buf, n, done) = (arg(e, 1)?, arg(e, 2)?, arg(e, 3)? as u32 as usize, arg(e, 4)?);
                    let mut bytes = vec![0u8; n];
                    e.mem.read(buf, &mut bytes)?;
                    {
                        let mut m = b.borrow_mut();
                        let s = m.entry(this).or_default();
                        if s.data.len() < s.pos + n {
                            s.data.resize(s.pos + n, 0);
                        }
                        s.data[s.pos..s.pos + n].copy_from_slice(&bytes);
                        s.pos += n;
                    }
                    if done != 0 {
                        e.mem.write_n(done, 4, n as u64)?;
                    }
                    Ok(K_RESULT_OK)
                }),
                // seek(pos, mode（0 頭から、1 今から、2 終わりから）, result*)
                Box::new(move |e: &mut Emu| {
                    let (this, pos, mode, result) = (arg(e, 1)?, arg(e, 2)? as i64, arg(e, 3)? as u32, arg(e, 4)?);
                    let now = {
                        let mut m = c.borrow_mut();
                        let s = m.entry(this).or_default();
                        let base = match mode {
                            1 => s.pos as i64,
                            2 => s.data.len() as i64,
                            _ => 0,
                        };
                        s.pos = (base + pos).max(0) as usize;
                        s.pos
                    };
                    if result != 0 {
                        e.mem.write_n(result, 8, now as u64)?;
                    }
                    Ok(K_RESULT_OK)
                }),
                // tell(pos*)
                Box::new(move |e: &mut Emu| {
                    let (this, at) = (arg(e, 1)?, arg(e, 2)?);
                    let now = d.borrow().get(&this).map(|s| s.pos).unwrap_or(0);
                    if at != 0 {
                        e.mem.write_n(at, 8, now as u64)?;
                    }
                    Ok(K_RESULT_OK)
                }),
            ],
        )?;
        Ok(Streams { table, all })
    }

    /// 中身 data のストリームを作る（位置は頭）
    pub fn open(&self, emu: &mut Emu, data: &[u8]) -> R<u64> {
        let obj = host_instance(emu, self.table, 0)?;
        self.all.borrow_mut().insert(obj, Stream { data: data.to_vec(), pos: 0 });
        Ok(obj)
    }

    /// ストリームの中身を取り出す（ストリームは捨てる）
    pub fn take(&self, obj: u64) -> Vec<u8> {
        self.all.borrow_mut().remove(&obj).map(|s| s.data).unwrap_or_default()
    }
}

/// 音の処理（component）とコントローラーを、IConnectionPoint::connect で両方からつなぐ。どちらかが持っていなければ何もしない
pub fn connect(emu: &mut Emu, component: u64, controller: u64) -> R<()> {
    let iid = emu.mem.alloc(16);
    emu.mem.write(iid, &IID_ICONNECTIONPOINT)?;
    let out = emu.mem.alloc(8);
    let mut point = |emu: &mut Emu, obj: u64| -> R<u64> {
        emu.mem.write_n(out, 8, 0)?;
        vcall(emu, obj, 0, &[Arg::Int(iid), Arg::Int(out)])?;
        emu.mem.read_n(out, 8)
    };
    let a = point(emu, component)?;
    let b = point(emu, controller)?;
    if a == 0 || b == 0 {
        return Ok(());
    }
    // connect(other)
    vcall(emu, a, 3, &[Arg::Int(b)])?;
    vcall(emu, b, 3, &[Arg::Int(a)])?;
    Ok(())
}
