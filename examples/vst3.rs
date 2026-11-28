//! VST3 の DLL を読み込み、クラスとつまみの一覧を出し、正弦波を通す。つまみ名（大文字小文字は区別しない）と値を渡すと、途中でそのつまみを変える。
//! cargo run --release --example vst3 -- <.vst3 の DLL> [秒] [つまみ名] [値]
use vstbridge::vst3::Vst3;
use vstbridge::vst3_process::Vst3Plugin;

fn rms(p: &mut Vst3Plugin, from: usize, blocks: usize) -> (f64, f64) {
    let n = 128;
    let (mut si, mut so) = (0f64, 0f64);
    for b in from..from + blocks {
        let x: Vec<f32> = (0..n).map(|i| ((b * n + i) as f32 * 2.0 * std::f32::consts::PI * 220.0 / 48000.0).sin() * 0.3).collect();
        let out = p.process(&[x.clone(), x.clone()], n).expect("process");
        si += x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
        so += out[0].iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
    }
    let t = (blocks * n) as f64;
    ((si / t).sqrt(), (so / t).sqrt())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dll = std::fs::read(&args[1]).expect("read");
    let secs: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let v = Vst3::load(&dll).expect("load");
    println!("vendor {:?}", v.vendor);
    for c in &v.classes {
        println!("  {:<28} {}", c.category, c.name);
    }
    let t = std::time::Instant::now();
    let mut p = Vst3Plugin::open(&dll, 48000.0).expect("open");
    println!("open（{} ms）入力 {} 出力 {}、つまみ {} 個", t.elapsed().as_millis(), p.inputs, p.outputs, p.params.len());
    for (i, q) in p.params.iter().take(16).enumerate() {
        println!("  {i:>3} id={:<10} {:<24} 既定 {:.3} 段階 {} 単位 {:?} flags {:#x}", q.id, q.title, q.default, q.steps, q.units, q.flags);
    }
    let blocks = (secs * 48000.0 / 128.0) as usize;
    let t = std::time::Instant::now();
    let (i0, o0) = rms(&mut p, 0, blocks);
    println!("既定のつまみ: RMS 入力 {i0:.4} 出力 {o0:.4}（{} ms）", t.elapsed().as_millis());
    if let (Some(name), Some(val)) = (args.get(3), args.get(4).and_then(|s| s.parse::<f64>().ok())) {
        let idx = p.params.iter().position(|q| q.title.eq_ignore_ascii_case(name)).expect("そのつまみが無い");
        p.set_param(idx, val).expect("set");
        let (i1, o1) = rms(&mut p, blocks, blocks);
        println!("{name} = {val}（読み返し {:.3}）: RMS 入力 {i1:.4} 出力 {o1:.4}", p.get_param(idx).expect("get"));
    }
}
