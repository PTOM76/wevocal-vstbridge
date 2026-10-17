//! 32 ビットの VST2 のエディター（画面）を開いて動かす。effEditGetRect、effEditOpen、effEditIdle、effEditClose と、マウスの入力、画面の取り出し
use crate::mem::{Fault, R};
use crate::vst32::Vst32;

const EFF_EDIT_GET_RECT: u64 = 13;
const EFF_EDIT_OPEN: u64 = 14;
const EFF_EDIT_CLOSE: u64 = 15;
const EFF_EDIT_IDLE: u64 = 19;

impl Vst32 {
    /// エディターがあるか（AEffect の flags の bit0）
    pub fn has_editor(&mut self) -> R<bool> {
        Ok(self.emu.mem.read_n(self.effect as u64 + 36, 4)? & 1 != 0)
    }

    /// effEditGetRect。ERect（上、左、下、右の 16 ビット）から (幅, 高さ)。無ければ 0
    fn edit_size(&mut self) -> R<(i32, i32)> {
        let buf = self.emu.mem.alloc32(16);
        self.dispatch(EFF_EDIT_GET_RECT, 0, 0, buf as u64, 0.0)?;
        let p = self.emu.mem.read_n(buf as u64, 4)?;
        if p == 0 {
            return Ok((0, 0));
        }
        let v: Vec<i32> = (0..4).map(|k| self.emu.mem.read_n(p + k * 2, 2).map(|x| x as u16 as i16 as i32)).collect::<R<_>>()?;
        Ok((v[3] - v[1], v[2] - v[0]))
    }

    /// エディターを開く。(幅, 高さ) を返す。画面は editor_frame で取り出す
    pub fn editor_open(&mut self) -> R<(i32, i32)> {
        if !self.has_editor()? {
            return Err(Fault("このプラグインには画面が無い".into()));
        }
        let (w, h) = self.edit_size()?;
        let root = self.emu.gui.create_root(w.max(1), h.max(1));
        self.emu.gui.base = self.base as u64;
        self.dispatch(EFF_EDIT_OPEN, 0, 0, root as u64, 0.0)?;
        // 開いたあとで大きさが決まるプラグインがある
        let (w2, h2) = self.edit_size()?;
        let (w, h) = if w2 > 0 && h2 > 0 { (w2, h2) } else { (w, h) };
        self.emu.gui.resize(root, w.max(1), h.max(1), 0xd4d0c8);
        self.emu.paint_all()?;
        Ok((w, h))
    }

    /// 一定時間ごとに呼ぶ（effEditIdle）。描き直しも行う
    pub fn editor_idle(&mut self) -> R<()> {
        // 中断した処理（つまみのドラッグ）があれば、その続きを動かす
        if self.emu.gui.pending && self.emu.resume_pending()? {
            return Ok(());
        }
        self.dispatch(EFF_EDIT_IDLE, 0, 0, 0, 0.0)?;
        self.emu.paint_all()
    }

    /// マウスの入力。msg は 0x200 移動、0x201 左ボタンを押した、0x202 離した、0x203 ダブルクリック。keys は bit0 左ボタン、bit1 Ctrl、bit2 Shift
    pub fn editor_mouse(&mut self, msg: u32, x: i32, y: i32, keys: u32) -> R<()> {
        self.emu.mouse_event(msg, x, y, keys)
    }

    /// 今の画面（幅, 高さ, RGBA）
    pub fn editor_frame(&self) -> Option<(i32, i32, Vec<u8>)> {
        self.emu.gui.frame()
    }

    pub fn editor_close(&mut self) -> R<()> {
        self.dispatch(EFF_EDIT_CLOSE, 0, 0, 0, 0.0)?;
        Ok(())
    }

    /// プラグインが「つまみを変えた」と知らせたもの（index, 値）を取り出す
    pub fn editor_changes(&mut self) -> Vec<(i32, f32)> {
        std::mem::take(&mut self.emu.gui.automation)
    }
}
