//! VST3 の画面（IPlugView）を、VST2 と同じ画面のエミュレーション（HWND、GDI）に付ける。
//! IEditController::createView("editor") で作り、ホストの窓（gui.create_root）に attached(HWND, "HWND") で付ける。
//! 画面でつまみを動かしたときの知らせは IComponentHandler::performEdit（vst3_process.rs）から gui.automation に積まれる
use crate::emu::{Arg, Emu};
use crate::mem::{Fault, R};
use crate::vst3::vcall;
use crate::vst3_process::{host_object, tuid, Vst3Plugin, K_RESULT_OK};

const IID_IPLUGFRAME: [u8; 16] = tuid(0x367FAF01, 0xAFA94693, 0x8D4DA2A0, 0xED0882A3);
/// 窓の背景（VST2 の画面と同じ）
const BG: u32 = 0xd4d0c8;

/// IPlugView の関数の番号（FUnknown の 3 つのあと）
mod view {
    pub const RELEASE: u64 = 2;
    pub const IS_PLATFORM_TYPE_SUPPORTED: u64 = 3;
    pub const ATTACHED: u64 = 4;
    pub const REMOVED: u64 = 5;
    pub const GET_SIZE: u64 = 9;
    pub const SET_FRAME: u64 = 12;
}
/// IEditController::createView
const CREATE_VIEW: u64 = 17;

/// 0 終わりの文字列をエミュレーターのメモリに置く
fn cstr(emu: &mut Emu, s: &str) -> R<u64> {
    let at = emu.mem.alloc(s.len() + 1);
    emu.mem.write(at, s.as_bytes())?;
    emu.mem.write(at + s.len() as u64, &[0])?;
    Ok(at)
}

impl Vst3Plugin {
    /// 画面を作る（まだなら）。作れなければ 0
    fn ensure_view(&mut self) -> R<u64> {
        if self.view != 0 || self.controller == 0 {
            return Ok(self.view);
        }
        let name = cstr(&mut self.emu, "editor")?;
        self.emu.max_steps = crate::emu::LOAD_MAX_STEPS;
        let v = vcall(&mut self.emu, self.controller, CREATE_VIEW, &[Arg::Int(name)]);
        self.emu.max_steps = crate::emu::MAX_STEPS;
        let v = v?;
        if v == 0 {
            return Ok(0);
        }
        let hwnd = cstr(&mut self.emu, "HWND")?;
        if vcall(&mut self.emu, v, view::IS_PLATFORM_TYPE_SUPPORTED, &[Arg::Int(hwnd)])? as u32 != K_RESULT_OK as u32 {
            vcall(&mut self.emu, v, view::RELEASE, &[])?;
            return Ok(0);
        }
        self.view = v;
        Ok(v)
    }

    /// 画面の大きさ（IPlugView::getSize。ViewRect は左、上、右、下の 32 ビット）
    fn view_size(&mut self) -> R<(i32, i32)> {
        let rect = self.emu.mem.alloc(16);
        self.emu.mem.write_n(rect, 8, 0)?;
        self.emu.mem.write_n(rect + 8, 8, 0)?;
        vcall(&mut self.emu, self.view, view::GET_SIZE, &[Arg::Int(rect)])?;
        let r: Vec<i32> = (0..4).map(|k| self.emu.mem.read_n(rect + k * 4, 4).map(|x| x as u32 as i32)).collect::<R<_>>()?;
        Ok((r[2] - r[0], r[3] - r[1]))
    }

    pub fn has_editor(&mut self) -> bool {
        self.ensure_view().map(|v| v != 0).unwrap_or(false)
    }

    /// 画面を開く。(幅, 高さ)
    pub fn editor_open(&mut self) -> R<(i32, i32)> {
        if self.ensure_view()? == 0 {
            return Err(Fault("このプラグインには画面が無い".into()));
        }
        // プラグインから大きさを変えたいとき（IPlugFrame::resizeView(view, rect)）は、窓の大きさを変える
        let root_cell = self.view_root.clone();
        let frame = host_object(
            &mut self.emu,
            vec![IID_IPLUGFRAME],
            vec![Box::new(move |e: &mut Emu| {
                let rect = crate::host_stubs::arg(e, 3)?;
                let r: Vec<i32> = (0..4).map(|k| e.mem.read_n(rect + k * 4, 4).map(|x| x as u32 as i32)).collect::<R<_>>()?;
                let root = root_cell.get();
                if root != 0 {
                    e.gui.resize(root, (r[2] - r[0]).max(1), (r[3] - r[1]).max(1), BG);
                }
                Ok(K_RESULT_OK)
            })],
        )?;
        vcall(&mut self.emu, self.view, view::SET_FRAME, &[Arg::Int(frame)])?;
        let (w, h) = self.view_size()?;
        let root = self.emu.gui.create_root(w.max(1), h.max(1));
        self.view_root.set(root);
        let hwnd = cstr(&mut self.emu, "HWND")?;
        self.emu.max_steps = crate::emu::LOAD_MAX_STEPS;
        let r = vcall(&mut self.emu, self.view, view::ATTACHED, &[Arg::Int(root as u64), Arg::Int(hwnd)]);
        self.emu.max_steps = crate::emu::MAX_STEPS;
        r?;
        // 付けたあとで大きさが決まるプラグインがある
        let (w2, h2) = self.view_size()?;
        let (w, h) = if w2 > 0 && h2 > 0 { (w2, h2) } else { (w, h) };
        self.emu.gui.resize(root, w.max(1), h.max(1), BG);
        self.emu.paint_all()?;
        Ok((w, h))
    }

    /// 一定時間ごと（30 回ほど毎秒）に呼ぶ。VST3 には idle が無いので、タイマーとメッセージを届けて描き直す
    pub fn editor_idle(&mut self) -> R<()> {
        if self.emu.gui.pending && self.emu.resume_pending()? {
            return Ok(());
        }
        self.emu.run_threads(33)?;
        self.emu.deliver_posted()?;
        self.emu.deliver_timers()?;
        self.emu.paint_all()
    }

    pub fn editor_mouse(&mut self, msg: u32, x: i32, y: i32, keys: u32) -> R<()> {
        self.emu.mouse_event(msg, x, y, keys)
    }

    pub fn editor_frame(&self) -> Option<(i32, i32, Vec<u8>)> {
        self.emu.gui.frame()
    }

    pub fn editor_changes(&mut self) -> Vec<(i32, f32)> {
        std::mem::take(&mut self.emu.gui.automation)
    }

    /// 画面を閉じる（removed、release）
    pub fn editor_close(&mut self) -> R<()> {
        if self.view != 0 {
            vcall(&mut self.emu, self.view, view::REMOVED, &[])?;
            vcall(&mut self.emu, self.view, view::RELEASE, &[])?;
            self.view = 0;
            self.view_root.set(0);
        }
        Ok(())
    }
}
