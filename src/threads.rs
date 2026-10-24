//! プラグインが作るスレッドを、交代で少しずつ動かす（本当の並行ではない）。スレッドごとに Cpu とスタックを持ち、
//! run_threads（エディターの idle）で順に入れ替えて一定の命令数まで動かす。待つ API（WaitForSingleObject、Sleep、条件変数）は
//! スレッドの中なら中断（YIELD）して、条件がそろったら続きから動かす。イベントは合図の有無を持つ。メインのスレッドの待ちはすぐ戻る（今までどおり）
use crate::cpu::{Cpu, RSP};
use crate::emu::{Emu, HostFn, RETURN_MAGIC, YIELD};
use crate::host_stubs::arg;
use crate::mem::R;
use std::collections::HashMap;

const WAIT_OBJECT_0: u64 = 0;
const WAIT_TIMEOUT: u64 = 0x102;
const INFINITE: u64 = 0xffff_ffff;
const STILL_ACTIVE: u64 = 259;
/// スレッドのスタックの大きさ
const STACK: usize = 1 << 20;
/// 1 回の run_threads で 1 つのスレッドを動かす命令数
const BUDGET: u64 = 2_000_000;
/// イベントとスレッドのハンドルの始まり（ほかの偽のハンドルと重ならない所）
const HANDLE_BASE: u64 = 0x6100_0000;

#[derive(Clone, Copy, PartialEq)]
enum Wait {
    Run,
    /// CREATE_SUSPENDED で作られ、ResumeThread を待つ
    Suspended,
    /// イベントかスレッドの終わり（ハンドル）を待つ。期限（時計の ms）があれば、過ぎたら WAIT_TIMEOUT
    Object(u64, Option<u64>),
    /// この時刻（ms）まで眠る
    Until(u64),
    Done,
}

struct Thread {
    cpu: Cpu,
    wait: Wait,
    handle: u64,
    id: u32,
}

#[derive(Default)]
pub struct Threads {
    list: Vec<Thread>,
    /// 今動かしているスレッドの添字（None はメインのスレッド）
    cur: Option<usize>,
    /// イベントのハンドル → (合図が立っているか, 手動で戻すか)
    events: HashMap<u64, (bool, bool)>,
    next: u64,
}

impl Threads {
    fn new_handle(&mut self) -> u64 {
        self.next += 4;
        HANDLE_BASE + self.next
    }
    /// ハンドルの合図が立っているか（スレッドなら終わったか）。自動で戻すイベントは、ここで戻す
    fn take_signal(&mut self, h: u64) -> bool {
        if let Some(t) = self.list.iter().find(|t| t.handle == h) {
            return t.wait == Wait::Done;
        }
        match self.events.get_mut(&h) {
            Some((on, manual)) if *on => {
                *on = *manual;
                true
            }
            Some(_) => false,
            // 知らないハンドル（今までの偽のハンドルなど）は、立っているものとする
            None => true,
        }
    }
}

/// 今の時刻（ms）。win_stubs の仮の時計と同じもの
fn now(e: &mut Emu) -> u64 {
    crate::win_stubs::clock_ms(e)
}

/// スレッドの中なら、条件を置いて中断する
fn block(e: &mut Emu, w: Wait) {
    if let Some(k) = e.threads.cur {
        e.threads.list[k].wait = w;
        e.yield_req = true;
    }
}

impl Emu {
    /// 動けるスレッドを順に動かす（エディターの idle から呼ぶ）。ms は前の回から進めた時計の分
    pub fn run_threads(&mut self, ms: u64) -> R<()> {
        crate::win_stubs::advance_clock(self, ms);
        for k in 0..self.threads.list.len() {
            let t = self.now_ms();
            let ready = match self.threads.list[k].wait {
                Wait::Run => Some(None),
                Wait::Until(at) if t >= at => Some(None),
                Wait::Object(h, dl) => {
                    if self.threads.take_signal(h) {
                        Some(Some(WAIT_OBJECT_0))
                    } else if dl.is_some_and(|d| t >= d) {
                        Some(Some(WAIT_TIMEOUT))
                    } else {
                        None
                    }
                }
                _ => None,
            };
            let Some(ret) = ready else { continue };
            if let Some(v) = ret {
                self.threads.list[k].cpu.regs[0] = v;
            }
            self.threads.list[k].wait = Wait::Run;
            // Cpu を入れ替えて動かす
            std::mem::swap(&mut self.cpu, &mut self.threads.list[k].cpu);
            let (saved_max, saved_start) = (self.max_steps, self.call_start);
            self.max_steps = BUDGET;
            self.threads.cur = Some(k);
            let r = self.resume();
            self.threads.cur = None;
            self.max_steps = saved_max;
            self.call_start = saved_start;
            std::mem::swap(&mut self.cpu, &mut self.threads.list[k].cpu);
            match r {
                Ok(()) => self.threads.list[k].wait = Wait::Done,
                // 待ちで止まった（条件は block が置いた）か、命令数を使い切った（次の回に続ける）
                Err(f) if f.0 == YIELD || f.0.starts_with("step limit") => {}
                Err(f) => {
                    eprintln!("[thread] {} が止まった: {}", self.threads.list[k].id, f.0);
                    self.threads.list[k].wait = Wait::Done;
                }
            }
        }
        Ok(())
    }

    fn now_ms(&mut self) -> u64 {
        now(self)
    }
}

fn boxed(f: impl Fn(&mut Emu) -> R<u64> + 'static) -> Option<HostFn> {
    Some(Box::new(f))
}

/// スレッドとイベントの API（KERNEL32）
pub fn resolve(name: &str) -> Option<HostFn> {
    match name {
        // CreateThread(attr, stack, start, param, flags, &id)。CREATE_SUSPENDED（4）なら ResumeThread まで動かさない
        "CreateThread" => boxed(|e| {
            let (start, param, flags, idp) = (arg(e, 3)?, arg(e, 4)?, arg(e, 5)?, arg(e, 6)?);
            let stack = e.mem.alloc32(STACK) as u64;
            let mut cpu = e.cpu.clone();
            cpu.steps = 0;
            cpu.jit_bail = 0;
            let mut sp = (stack + STACK as u64 - 64) & !15;
            if e.bits() == 64 {
                // 入口では rsp が 16 の倍数 + 8（戻り先を積んだあと）。引数は rcx、上に 32 バイトの影の領域
                sp -= 32;
                cpu.regs[1] = param;
            } else {
                sp -= 4;
                e.mem.write_n(sp, 4, param)?;
            }
            let w = if e.bits() == 64 { 8 } else { 4 };
            sp -= w;
            e.mem.write_n(sp, w as usize, RETURN_MAGIC)?;
            cpu.regs[RSP] = sp;
            cpu.rip = start;
            // TEB はメインのものを写し、スタックの範囲と自分を指す値だけ変える（TLS と PEB はメインと同じものを指す）
            let teb = e.mem.alloc32(0x1000) as u64;
            let mut page = vec![0u8; 0x1000];
            e.mem.read(e.cpu.fs_base, &mut page)?;
            e.mem.write(teb, &page)?;
            let top = stack + STACK as u64;
            if e.bits() == 64 {
                e.mem.write_n(teb + 8, 8, top)?;
                e.mem.write_n(teb + 0x10, 8, stack)?;
                e.mem.write_n(teb + 0x30, 8, teb)?;
            } else {
                e.mem.write_n(teb, 4, 0xffff_ffff)?;
                e.mem.write_n(teb + 4, 4, top)?;
                e.mem.write_n(teb + 8, 4, stack)?;
                e.mem.write_n(teb + 0x18, 4, teb)?;
            }
            cpu.fs_base = teb;
            let handle = e.threads.new_handle();
            let id = 0x100 + e.threads.list.len() as u32;
            let wait = if flags & 4 != 0 { Wait::Suspended } else { Wait::Run };
            e.threads.list.push(Thread { cpu, wait, handle, id });
            if idp != 0 {
                e.mem.write_n(idp, 4, id as u64)?;
            }
            Ok(handle)
        }),
        "ResumeThread" => boxed(|e| {
            let h = arg(e, 1)?;
            if let Some(t) = e.threads.list.iter_mut().find(|t| t.handle == h && t.wait == Wait::Suspended) {
                t.wait = Wait::Run;
                return Ok(1);
            }
            Ok(0)
        }),
        "GetCurrentThreadId" => boxed(|e| Ok(e.threads.cur.map(|k| e.threads.list[k].id as u64).unwrap_or(1))),
        "GetExitCodeThread" => boxed(|e| {
            let (h, p) = (arg(e, 1)?, arg(e, 2)?);
            let done = e.threads.list.iter().find(|t| t.handle == h).is_none_or(|t| t.wait == Wait::Done);
            e.mem.write_n(p, 4, if done { 0 } else { STILL_ACTIVE })?;
            Ok(1)
        }),
        // CreateEvent(attr, 手動で戻すか, 最初の状態, 名前)
        "CreateEventW" | "CreateEventA" => boxed(|e| {
            let (manual, init) = (arg(e, 2)? as u32 != 0, arg(e, 3)? as u32 != 0);
            let h = e.threads.new_handle();
            e.threads.events.insert(h, (init, manual));
            Ok(h)
        }),
        // CreateEventEx(attr, 名前, flags, access)。flags の 1 は手動で戻す、2 は最初から立っている
        "CreateEventExW" | "CreateEventExA" => boxed(|e| {
            let fl = arg(e, 3)?;
            let h = e.threads.new_handle();
            e.threads.events.insert(h, (fl & 2 != 0, fl & 1 != 0));
            Ok(h)
        }),
        "SetEvent" | "ResetEvent" => {
            let on = name == "SetEvent";
            boxed(move |e| {
                if let Some(ev) = e.threads.events.get_mut(&arg(e, 1)?) {
                    ev.0 = on;
                }
                Ok(1)
            })
        }
        // メインのスレッドはすぐ戻る（合図が立っていれば戻す）。スレッドの中では、立っていなければ中断して待つ
        "WaitForSingleObject" | "WaitForSingleObjectEx" => boxed(|e| {
            let (h, ms) = (arg(e, 1)?, arg(e, 2)? & 0xffff_ffff);
            if e.threads.take_signal(h) {
                return Ok(WAIT_OBJECT_0);
            }
            if e.threads.cur.is_none() {
                return Ok(WAIT_OBJECT_0);
            }
            if ms == 0 {
                return Ok(WAIT_TIMEOUT);
            }
            let dl = (ms != INFINITE).then(|| now(e) + ms);
            block(e, Wait::Object(h, dl));
            // 戻り値は、再開するときに run_threads が入れる
            Ok(WAIT_TIMEOUT)
        }),
        "Sleep" | "SleepEx" => boxed(|e| {
            let ms = arg(e, 1)? & 0xffff_ffff;
            let at = now(e) + ms;
            block(e, Wait::Until(at));
            Ok(0)
        }),
        // 条件変数の待ちは、偽の起床（許されている）として 1 回分だけ眠って戻る。ロックはスレッドが交代しないので離さなくてよい
        "SleepConditionVariableSRW" | "SleepConditionVariableCS" => boxed(|e| {
            let ms = arg(e, 3)? & 0xffff_ffff;
            let at = now(e) + ms.min(1);
            block(e, Wait::Until(at));
            Ok(1)
        }),
        "SwitchToThread" => boxed(|e| {
            let at = now(e);
            block(e, Wait::Until(at));
            Ok(1)
        }),
        _ => None,
    }
}
