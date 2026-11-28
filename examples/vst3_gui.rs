//! VST3 の画面を開き、何回か idle を回して、描かれた画を PPM に書く。
//! cargo run --release --example vst3_gui -- <.vst3 の DLL> <出力の .ppm>
use vstbridge::vst3_process::Vst3Plugin;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dll = std::fs::read(&args[1]).expect("read");
    let mut p = Vst3Plugin::open(&dll, 48000.0).expect("open");
    println!("画面 {}", p.has_editor());
    let t = std::time::Instant::now();
    let (w, h) = p.editor_open().expect("editor_open");
    println!("開いた {w}x{h}（{} ms）", t.elapsed().as_millis());
    for _ in 0..10 {
        p.editor_idle().expect("idle");
    }
    let (fw, fh, px) = p.editor_frame().expect("frame");
    let lit = px.chunks(4).filter(|c| c[0] as u32 + c[1] as u32 + c[2] as u32 > 30).count();
    println!("画 {fw}x{fh}、明るい画素 {lit} / {}", px.len() / 4);
    let mut out = format!("P6\n{fw} {fh}\n255\n").into_bytes();
    for c in px.chunks(4) {
        out.extend_from_slice(&c[..3]);
    }
    std::fs::write(&args[2], out).expect("write");
    // 画面の中央をドラッグして、つまみの変化が届くか
    p.editor_mouse(0x201, fw / 2, fh / 2, 1).expect("down");
    for k in 1..20 {
        p.editor_mouse(0x200, fw / 2, fh / 2 - k * 3, 1).expect("move");
        p.editor_idle().expect("idle");
    }
    p.editor_mouse(0x202, fw / 2, fh / 2 - 60, 0).expect("up");
    p.editor_idle().expect("idle");
    println!("つまみの変化 {:?}", p.editor_changes().iter().take(5).collect::<Vec<_>>());
    p.editor_close().expect("close");
}
