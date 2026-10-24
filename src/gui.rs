//! プラグインのエディター（画面）の土台。Windows のウィンドウ（クラス、親子、描く面）、マウスなどの入力、メッセージの送り、画面の合成。
//! 描画そのものは gdi.rs、ビットマップのリソースは rsrc.rs、Windows の API の入口は gui_api.rs
use crate::emu::Emu;
use crate::gdi::{Bitmap, Gdi, Obj};
use crate::mem::R;
use std::collections::HashMap;

pub const WM_PAINT: u32 = 0x0F;
pub const WM_DESTROY: u32 = 0x02;

pub struct Window {
    pub class: String,
    pub parent: u32,
    /// 親の中での位置と大きさ
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// 描く面（ビットマップのハンドル）
    pub surface: u32,
    /// GetWindowLong、SetWindowLong で出し入れする値（添字 → 値。-4 はウィンドウプロシージャ、-21 は利用者のデータ）
    pub longs: HashMap<i32, u64>,
}

pub struct Class {
    pub atom: u32,
    pub wndproc: u32,
    pub bg: u32,
}

pub struct Gui {
    pub gdi: Gdi,
    pub windows: HashMap<u32, Window>,
    /// 作った順（描くときの重なり。あとのものが上）
    pub order: Vec<u32>,
    pub classes: HashMap<String, Class>,
    next: u32,
    /// 仮想のカーソルの位置（画面の座標。ルートのウィンドウの左上が原点）。SetCursorPos で動かせる
    pub cursor: (i32, i32),
    /// 最後に届いたポインターの位置（動いた分を仮想のカーソルに足すため）
    pub pointer: (i32, i32),
    pub cursor_count: i32,
    /// 押されているキー（bit0 左ボタン、bit1 Ctrl、bit2 Shift、bit3 Alt）
    pub keys: u32,
    /// プラグインが「つまみを変えた」と知らせた（index, 0〜1 の値）。ホストが取り出して音の側へ渡す
    pub automation: Vec<(i32, f32)>,
    pub root: u32,
    /// 入力のメッセージの処理を動かしている間 true（押している間ずっと待つループのところで、中断してよい）
    pub can_yield: bool,
    /// 中断したままの処理がある（次の入力か idle で resume する）
    pub pending: bool,
    /// リソースを探すイメージの基点
    pub base: u64,
    /// CreateDIBSection で作ったビットマップ → (画素のゲストの番地, 幅, 高さ, 上の行から並ぶか, 1 画素のビット数)。描く前にゲストのメモリから写す
    pub dibs: HashMap<u32, (u64, i32, i32, bool, u32)>,
    /// PostMessage で積まれたメッセージ（ウィンドウ, メッセージ, wParam, lParam）。idle のときに届ける
    pub posted: Vec<(u32, u32, u64, u64)>,
    /// プラグインが渡したフォント（文字の形を答えるのに使う）
    pub fonts: crate::fonts::Fonts,
}

impl Gui {
    pub fn new() -> Gui {
        Gui { gdi: Gdi::new(), windows: HashMap::new(), order: Vec::new(), classes: HashMap::new(), next: 0x0003_0000, cursor: (0, 0), pointer: (0, 0), cursor_count: 0, keys: 0, automation: Vec::new(), root: 0, can_yield: false, pending: false, base: 0x1000_0000, dibs: HashMap::new(), posted: Vec::new(), fonts: Default::default() }
    }

    /// ウィンドウを作る（描く面は bg で塗る）。ハンドルを返す
    pub fn add_window(&mut self, class: &str, parent: u32, x: i32, y: i32, w: i32, h: i32, bg: u32) -> u32 {
        self.next += 4;
        let hwnd = self.next;
        let surface = self.gdi.add(Obj::Bitmap(Bitmap::new(w, h, bg)));
        self.windows.insert(hwnd, Window { class: class.to_string(), parent, x, y, w, h, surface, longs: HashMap::new() });
        self.order.push(hwnd);
        hwnd
    }

    /// ホストが用意する、プラグインの画面を載せるウィンドウ（VST の effEditOpen に渡す親）
    pub fn create_root(&mut self, w: i32, h: i32) -> u32 {
        let hwnd = self.add_window("", 0, 0, 0, w, h, 0xd4d0c8);
        self.root = hwnd;
        hwnd
    }

    pub fn resize(&mut self, hwnd: u32, w: i32, h: i32, bg: u32) {
        if let Some(win) = self.windows.get_mut(&hwnd) {
            win.w = w;
            win.h = h;
            let s = win.surface;
            self.gdi.objs.insert(s, Obj::Bitmap(Bitmap::new(w, h, bg)));
        }
    }

    pub fn remove_window(&mut self, hwnd: u32) {
        if let Some(w) = self.windows.remove(&hwnd) {
            self.gdi.objs.remove(&w.surface);
        }
        self.order.retain(|&h| h != hwnd);
    }

    /// ウィンドウの左上の画面での位置
    pub fn origin(&self, hwnd: u32) -> (i32, i32) {
        let mut p = (0, 0);
        let mut cur = hwnd;
        while let Some(w) = self.windows.get(&cur) {
            p = (p.0 + w.x, p.1 + w.y);
            cur = w.parent;
        }
        p
    }

    /// 画面の位置 (x, y) にある一番上の子ウィンドウと、その中の位置。無ければルート
    pub fn hit(&self, x: i32, y: i32) -> (u32, i32, i32) {
        for &h in self.order.iter().rev() {
            if h == self.root {
                continue;
            }
            let Some(w) = self.windows.get(&h) else { continue };
            let (ox, oy) = self.origin(h);
            if x >= ox && y >= oy && x < ox + w.w && y < oy + w.h {
                return (h, x - ox, y - oy);
            }
        }
        (self.root, x, y)
    }

    /// 画面に出す絵（RGBA）。ルートの面に、子ウィンドウを作った順に重ねる
    pub fn frame(&self) -> Option<(i32, i32, Vec<u8>)> {
        let root = self.windows.get(&self.root)?;
        let mut canvas = self.gdi.bitmap(root.surface)?.clone();
        for &h in &self.order {
            if h == self.root {
                continue;
            }
            let Some(w) = self.windows.get(&h) else { continue };
            let Some(src) = self.gdi.bitmap(w.surface) else { continue };
            let (ox, oy) = self.origin(h);
            for y in 0..src.h {
                for x in 0..src.w {
                    canvas.set(ox + x, oy + y, src.px[(y * src.w + x) as usize]);
                }
            }
        }
        let mut out = Vec::with_capacity(canvas.px.len() * 4);
        for p in &canvas.px {
            out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]);
        }
        Some((canvas.w, canvas.h, out))
    }
}

impl Default for Gui {
    fn default() -> Self {
        Self::new()
    }
}

impl Emu {
    /// ウィンドウにメッセージを送り、ウィンドウプロシージャ（プラグインの関数）の戻り値を返す。プロシージャが無ければ 0
    pub fn send_message(&mut self, hwnd: u32, msg: u32, wparam: u32, lparam: u32) -> R<u64> {
        let Some(win) = self.gui.windows.get(&hwnd) else { return Ok(0) };
        let proc = win.longs.get(&-4).copied().or_else(|| self.gui.classes.get(&win.class).map(|c| c.wndproc as u64)).unwrap_or(0);
        if proc == 0 {
            return Ok(0);
        }
        use crate::emu::Arg;
        let args = [Arg::Int(hwnd as u64), Arg::Int(msg as u64), Arg::Int(wparam as u64), Arg::Int(lparam as u64)];
        if self.bits() == 64 { self.call(proc, &args) } else { self.call32(proc as u32, &args) }
    }

    /// PostMessage で積まれたメッセージを届ける（届ける間に積まれた分は次の回）
    pub fn deliver_posted(&mut self) -> R<()> {
        for (h, m, w, l) in std::mem::take(&mut self.gui.posted) {
            self.send_message(h, m, w as u32, l as u32)?;
        }
        Ok(())
    }

    /// 子ウィンドウ全部に WM_PAINT を送る（描き直し）
    pub fn paint_all(&mut self) -> R<()> {
        for h in self.gui.order.clone() {
            if h != self.gui.root && self.gui.windows.contains_key(&h) {
                self.send_message(h, WM_PAINT, 0, 0)?;
            }
        }
        Ok(())
    }

    /// マウスなどの入力を届ける。msg は WM_MOUSEMOVE(0x200)、WM_LBUTTONDOWN(0x201)、WM_LBUTTONUP(0x202)、WM_LBUTTONDBLCLK(0x203)。(x, y) はルートの左上からの位置
    pub fn mouse_event(&mut self, msg: u32, x: i32, y: i32, keys: u32) -> R<()> {
        let g = &mut self.gui;
        // 仮想のカーソルは、ポインターが動いた分だけ動く（プラグインが SetCursorPos で戻した分は保たれる）
        g.cursor = (g.cursor.0 + x - g.pointer.0, g.cursor.1 + y - g.pointer.1);
        g.pointer = (x, y);
        g.keys = keys;
        // 押している間ずっと待つループ（つまみのドラッグ）で中断していたなら、続きを動かす。ループが終わったら、今回のメッセージも届ける
        if self.gui.pending && self.resume_pending()? {
            return Ok(());
        }
        let (hwnd, lx, ly) = self.gui.hit(x, y);
        // MK_LBUTTON=1、MK_SHIFT=4、MK_CONTROL=8
        let mk = (keys & 1) | ((keys >> 1 & 1) << 3) | ((keys >> 2 & 1) << 2);
        self.gui.can_yield = true;
        let r = self.send_message(hwnd, msg, mk, ((ly as u32 & 0xffff) << 16) | (lx as u32 & 0xffff));
        self.gui.can_yield = false;
        match r {
            Ok(_) => self.paint_all(),
            Err(f) if f.0 == crate::emu::YIELD => {
                self.gui.pending = true;
                Ok(())
            }
            Err(f) => Err(f),
        }
    }

    /// 中断した処理を動かす。まだ終わらなければ true（また中断した）。終わったら false
    pub fn resume_pending(&mut self) -> R<bool> {
        self.gui.can_yield = true;
        let r = self.resume();
        self.gui.can_yield = false;
        match r {
            Ok(()) => {
                self.gui.pending = false;
                self.paint_all()?;
                Ok(false)
            }
            Err(f) if f.0 == crate::emu::YIELD => Ok(true),
            Err(f) => {
                self.gui.pending = false;
                Err(f)
            }
        }
    }
}
