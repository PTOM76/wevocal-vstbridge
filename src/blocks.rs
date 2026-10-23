//! 基本ブロック（分岐まで一直線の命令の列）を前もって Op にして覚え、ブロックごとに動かす。
//! 命令ごとに表を引く手間をなくし、同じブロックを何度も回すループを速くする
use crate::emu::{Emu, RETURN_MAGIC, THUNK_BASE};
use crate::fast::{self, Op};
use crate::mem::{Fault, R};
use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction};
use std::collections::HashMap;

/// ブロックの中の命令 1 つ
#[derive(Clone, Copy)]
struct Item {
    op: Op,
    /// この命令の次の番地（32 ビットのコードだけ）
    next: u32,
    /// Op::Generic のとき、generics の中の添字
    gi: u32,
}

/// 直接引きの表の大きさ（2 のべき乗）
const SLOTS: usize = 4096;
/// 1 ブロックの命令数の上限
const MAX_BLOCK: usize = 96;

pub struct Blocks {
    code: Vec<Item>,
    generics: Vec<Instruction>,
    /// 番地 → (code の始まり, 命令数)
    map: HashMap<u64, (u32, u32), crate::emu::FastHash>,
    /// 直接引きの表（番地, 始まり, 命令数）。番地が 0 のところは空
    slots: Vec<(u64, u32, u32)>,
}

impl Default for Blocks {
    fn default() -> Self {
        Blocks { code: Vec::new(), generics: Vec::new(), map: HashMap::default(), slots: vec![(u64::MAX, 0, 0); SLOTS] }
    }
}

impl Blocks {
    /// 覚えたブロックを全部捨てる（コードの書き換えがあったとき）
    pub fn clear(&mut self) {
        self.code.clear();
        self.generics.clear();
        self.map.clear();
        self.slots.iter_mut().for_each(|s| s.0 = u64::MAX);
    }
}

#[inline(always)]
fn slot(rip: u64) -> usize {
    ((rip ^ (rip >> 12)) as usize) & (SLOTS - 1)
}

impl Emu {
    #[cfg(target_arch = "wasm32")]
    /// rip の命令を jit_miss に数える（JIT で動かせなかった理由を調べるため）。bail は JIT の途中で遅い道に回った命令
    fn count_miss(&mut self, bail: bool) {
        let rip = self.cpu.rip;
        let Ok(bytes) = self.mem.fetch(rip, 16) else { return };
        let insn = Decoder::with_ip(self.bits(), bytes, rip, DecoderOptions::NONE).decode();
        let key = format!("{}{insn}", if bail { "bail " } else { "" });
        *self.jit_miss.entry(key).or_insert(0) += 1;
    }

    /// rip から始まるブロック（code の始まり, 命令数）。なければ作る
    #[inline(always)]
    fn block_at(&mut self, rip: u64) -> R<(u32, u32)> {
        let s = self.blocks.slots[slot(rip)];
        if s.0 == rip {
            return Ok((s.1, s.2));
        }
        let b = match self.blocks.map.get(&rip) {
            Some(b) => *b,
            None => self.build_block(rip)?,
        };
        self.blocks.slots[slot(rip)] = (rip, b.0, b.1);
        Ok(b)
    }

    fn build_block(&mut self, start: u64) -> R<(u32, u32)> {
        let first = self.blocks.code.len() as u32;
        let mut rip = start;
        loop {
            let bytes = match self.mem.fetch(rip, 16) {
                Ok(b) => b,
                Err(e) if rip == start => return Err(e),
                Err(_) => break,
            };
            let mut dec = Decoder::with_ip(self.bits(), bytes, rip, DecoderOptions::NONE);
            let insn = dec.decode();
            if insn.is_invalid() {
                if rip == start {
                    return Err(Fault(format!("decode {rip:#x}")));
                }
                break;
            }
            let op = fast::compile(&insn);
            let mut gi = 0;
            if matches!(op, Op::Generic) {
                gi = self.blocks.generics.len() as u32;
                self.blocks.generics.push(insn);
            }
            self.blocks.code.push(Item { op, next: insn.next_ip() as u32, gi });
            rip = insn.next_ip();
            let n = self.blocks.code.len() - first as usize;
            // 分岐、汎用の実装で動かす命令（rip を変えうる）、上限、ホストの入口の手前でブロックを区切る
            let ends = matches!(op, Op::Generic) || insn.flow_control() != FlowControl::Next || n >= MAX_BLOCK || (rip >= THUNK_BASE && rip < self.next_thunk()) || rip == RETURN_MAGIC;
            if ends {
                break;
            }
        }
        let b = (first, self.blocks.code.len() as u32 - first);
        self.blocks.map.insert(start, b);
        Ok(b)
    }

    /// ブロック単位で実行する（rip から、戻りの印に来るまで）
    pub(crate) fn run_blocks(&mut self) -> R<()> {
        self.call_start = self.cpu.steps;
        loop {
            let rip = self.cpu.rip;
            if rip == RETURN_MAGIC {
                return Ok(());
            }
            if self.try_thunk(rip)? {
                if self.yield_req {
                    self.yield_req = false;
                    return Err(Fault(crate::emu::YIELD.into()));
                }
                continue;
            }
            if self.mem.code_dirty {
                self.mem.code_dirty = false;
                self.blocks.clear();
                self.decoded_clear();
                self.jit.clear();
            }
            // JIT: 何度も通ったブロックは wasm に変換して直接呼ぶ（wasm の中だけ）
            #[cfg(target_arch = "wasm32")]
            if self.jit.on {
                let bits = self.bits();
                if let Some(f) = self.jit.get(&self.mem, bits, rip, 256, &self.cpu.regs) {
                    let cpu_ptr = (&mut self.cpu as *mut crate::cpu::Cpu) as usize as u32;
                    let n = f(cpu_ptr);
                    self.cpu.steps += n as u64;
                    self.jit_insns += n as u64;
                    if self.cpu.jit_bail != 0 {
                        // メモリの遅い道。その 1 命令だけインタープリターで進める
                        self.cpu.jit_bail = 0;
                        if self.jit.count_miss {
                            self.count_miss(true);
                        }
                        self.step()?;
                        self.cpu.steps += 1;
                    }
                    if self.cpu.steps - self.call_start > self.max_steps {
                        return Err(Fault(format!("step limit (rip={:#x})", self.cpu.rip)));
                    }
                    continue;
                }
            }
            let (start, len) = self.block_at(rip)?;
            #[cfg(target_arch = "wasm32")]
            if self.jit.on && self.jit.count_miss {
                self.count_miss(false);
            }
            let first = start as usize;
            let last = (start + len) as usize - 1;
            // 分岐しなければブロックの次へ進む（分岐の命令は中で rip を書き換える）
            self.cpu.rip = self.blocks.code[last].next as u64;
            let mut done = 0u64;
            for k in first..=last {
                let it = self.blocks.code[k];
                done += 1;
                let r = match it.op {
                    Op::Generic => {
                        let insn = self.blocks.generics[it.gi as usize];
                        self.run_insn(&insn).map_err(|e| Fault(format!("{} (at {:#x}: {insn})", e.0, insn.ip())))
                    }
                    op => fast::run(&mut self.cpu, &mut self.mem, op).map_err(|e| Fault(format!("{} (ブロック {rip:#x} の {} 番目)", e.0, k - first))),
                };
                if let Err(e) = r {
                    self.cpu.steps += done;
                    return Err(e);
                }
                if self.mem.code_dirty {
                    // コードの書き換え。次の命令から続ける（ブロックは捨てられる）
                    if k < last {
                        self.cpu.rip = it.next as u64;
                    }
                    break;
                }
            }
            self.cpu.steps += done;
            self.interp_insns += done;
            if self.cpu.steps - self.call_start > self.max_steps {
                return Err(Fault(format!("step limit (rip={:#x})", self.cpu.rip)));
            }
        }
    }
}
