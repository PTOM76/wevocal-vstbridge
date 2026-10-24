//! VST2 のエディター（画面）を開いて動かす（32 と 64 ビット）。effEditGetRect、effEditOpen、effEditIdle、effEditClose と、マウスの入力、画面の取り出し
use crate::emu::Emu;
use crate::mem::{Fault, R};
use crate::vst::Vst;
use crate::vst32::Vst32;

const EFF_EDIT_GET_RECT: u64 = 13;
const EFF_EDIT_OPEN: u64 = 14;
const EFF_EDIT_CLOSE: u64 = 15;
const EFF_EDIT_IDLE: u64 = 19;

/// エディターを動かすのに要るもの（32 と 64 ビットの違いだけ）
pub trait EditorHost {
    fn emu(&mut self) -> &mut Emu;
    fn emu_ref(&self) -> &Emu;
    fn dispatch_e(&mut self, opcode: u64, index: u64, value: u64, ptr: u64, opt: f32) -> R<u64>;
    /// AEffect の flags の番地
    fn flags_addr(&self) -> u64;
    /// ポインターの幅（バイト）
    fn ptr_size(&self) -> usize;
}

impl EditorHost for Vst32 {
    fn emu(&mut self) -> &mut Emu {
        &mut self.emu
    }
    fn emu_ref(&self) -> &Emu {
        &self.emu
    }
    fn dispatch_e(&mut self, opcode: u64, index: u64, value: u64, ptr: u64, opt: f32) -> R<u64> {
        self.dispatch(opcode, index, value, ptr, opt)
    }
    fn flags_addr(&self) -> u64 {
        self.effect as u64 + 36
    }
    fn ptr_size(&self) -> usize {
        4
    }
}

impl EditorHost for Vst {
    fn emu(&mut self) -> &mut Emu {
        &mut self.emu
    }
    fn emu_ref(&self) -> &Emu {
        &self.emu
    }
    fn dispatch_e(&mut self, opcode: u64, index: u64, value: u64, ptr: u64, opt: f32) -> R<u64> {
        self.dispatch(opcode, index, value, ptr, opt)
    }
    fn flags_addr(&self) -> u64 {
        self.effect + 56
    }
    fn ptr_size(&self) -> usize {
        8
    }
}

pub trait Editor: EditorHost {
    /// エディターがあるか（AEffect の flags の bit0）
    fn has_editor(&mut self) -> R<bool> {
        let a = self.flags_addr();
        Ok(self.emu().mem.read_n(a, 4)? & 1 != 0)
    }

    /// effEditGetRect。ERect（上、左、下、右の 16 ビット）から (幅, 高さ)。無ければ 0
    fn edit_size(&mut self) -> R<(i32, i32)> {
        let buf = self.emu().mem.alloc32(16) as u64;
        self.emu().mem.write_n(buf, 8, 0)?;
        self.dispatch_e(EFF_EDIT_GET_RECT, 0, 0, buf, 0.0)?;
        let w = self.ptr_size();
        let p = self.emu().mem.read_n(buf, w)?;
        if p == 0 {
            return Ok((0, 0));
        }
        let v: Vec<i32> = (0..4).map(|k| self.emu().mem.read_n(p + k * 2, 2).map(|x| x as u16 as i16 as i32)).collect::<R<_>>()?;
        Ok((v[3] - v[1], v[2] - v[0]))
    }

    /// エディターを開く。(幅, 高さ) を返す。画面は editor_frame で取り出す
    fn editor_open(&mut self) -> R<(i32, i32)> {
        if !self.has_editor()? {
            return Err(Fault("このプラグインには画面が無い".into()));
        }
        let (w, h) = self.edit_size()?;
        let root = self.emu().gui.create_root(w.max(1), h.max(1));
        self.dispatch_e(EFF_EDIT_OPEN, 0, 0, root as u64, 0.0)?;
        // 開いたあとで大きさが決まるプラグインがある
        let (w2, h2) = self.edit_size()?;
        let (w, h) = if w2 > 0 && h2 > 0 { (w2, h2) } else { (w, h) };
        self.emu().gui.resize(root, w.max(1), h.max(1), 0xd4d0c8);
        self.emu().paint_all()?;
        Ok((w, h))
    }

    /// 一定時間ごとに呼ぶ（effEditIdle）。描き直しも行う
    fn editor_idle(&mut self) -> R<()> {
        // 中断した処理（つまみのドラッグ）があれば、その続きを動かす
        if self.emu().gui.pending && self.emu().resume_pending()? {
            return Ok(());
        }
        self.emu().deliver_posted()?;
        self.dispatch_e(EFF_EDIT_IDLE, 0, 0, 0, 0.0)?;
        self.emu().paint_all()
    }

    /// マウスの入力。msg は 0x200 移動、0x201 左ボタンを押した、0x202 離した、0x203 ダブルクリック。keys は bit0 左ボタン、bit1 Ctrl、bit2 Shift
    fn editor_mouse(&mut self, msg: u32, x: i32, y: i32, keys: u32) -> R<()> {
        self.emu().mouse_event(msg, x, y, keys)
    }

    /// 今の画面（幅, 高さ, RGBA）
    fn editor_frame(&self) -> Option<(i32, i32, Vec<u8>)> {
        self.emu_ref().gui.frame()
    }

    fn editor_close(&mut self) -> R<()> {
        self.dispatch_e(EFF_EDIT_CLOSE, 0, 0, 0, 0.0)?;
        Ok(())
    }

    /// プラグインが「つまみを変えた」と知らせたもの（index, 値）を取り出す
    fn editor_changes(&mut self) -> Vec<(i32, f32)> {
        std::mem::take(&mut self.emu().gui.automation)
    }
}

impl Editor for Vst32 {}
impl Editor for Vst {}
