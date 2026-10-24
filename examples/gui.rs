//! VST2 のエディター（画面）を開いて、絵を PPM で書き出す。使い方: cargo run --release --example gui -- <dll> <出力の .ppm> [マウス操作: 種類,x,y,keys ...]
//! 種類は move、down、up。VSTBRIDGE_CALLS=1 で、未対応の API の呼び出しを表示する
use vstbridge::vst::Vst;
use vstbridge::vst32::Vst32;
use vstbridge::vst32_gui::Editor;

fn save(v: &dyn Editor, path: &str) {
    let (w, h, rgba) = v.editor_frame().expect("frame");
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for px in rgba.chunks(4) {
        out.extend_from_slice(&px[..3]);
    }
    std::fs::write(path, out).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).expect("read");
    // PE の Machine で 32 か 64 ビットかを見分ける
    let pe = u32::from_le_bytes(data[0x3c..0x40].try_into().unwrap()) as usize;
    let mut v: Box<dyn Editor> = if data[pe + 4..pe + 6] == [0x64, 0x86] { Box::new(Vst::load(&data).expect("load")) } else { Box::new(Vst32::load(&data).expect("load")) };
    let (w, h) = v.editor_open().expect("open");
    println!("画面 {w} x {h}");
    // 非同期の処理（PostMessage）と描き直しを進める
    for _ in 0..std::env::var("IDLE").ok().and_then(|n| n.parse().ok()).unwrap_or(0) {
        v.editor_idle().expect("idle");
    }
    save(v.as_ref(), &args[2]);
    for (n, op) in args.iter().skip(3).enumerate() {
        let p: Vec<&str> = op.split(',').collect();
        let msg = match p[0] {
            "move" => 0x200,
            "down" => 0x201,
            _ => 0x202,
        };
        let keys = p.get(3).and_then(|k| k.parse().ok()).unwrap_or(if p[0] == "up" { 0 } else { 1 });
        v.editor_mouse(msg, p[1].parse().unwrap(), p[2].parse().unwrap(), keys).expect("mouse");
        v.editor_idle().expect("idle");
        println!("{op} → つまみの変更 {:?}", v.editor_changes());
        save(v.as_ref(), &format!("{}.{}.ppm", args[2].trim_end_matches(".ppm"), n + 1));
    }
}
