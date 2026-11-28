//! VST3 の DLL を読み込み、クラスの一覧を出し、音を処理するクラスで正弦波を通す（段階 1、2 の確かめ）。cargo run --release --example vst3 -- <.vst3 の DLL> [秒]
use vstbridge::vst3::Vst3;
use vstbridge::vst3_process::Vst3Plugin;

fn main() {
    let path = std::env::args().nth(1).expect("path");
    let secs: f64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let dll = std::fs::read(&path).expect("read");
    let t = std::time::Instant::now();
    let v = Vst3::load(&dll).expect("load");
    println!("vendor {:?}（{} ms）", v.vendor, t.elapsed().as_millis());
    for c in &v.classes {
        println!("  {:<28} {:<36} {}", c.category, c.name, c.cid.iter().map(|b| format!("{b:02X}")).collect::<String>());
    }
    let t = std::time::Instant::now();
    let mut p = Vst3Plugin::open(&dll, 48000.0).expect("open");
    println!("open（{} ms）入力 {} 出力 {}", t.elapsed().as_millis(), p.inputs, p.outputs);
    let n = 128;
    let blocks = (secs * 48000.0 / n as f64) as usize;
    let (mut sum_in, mut sum_out) = (0f64, 0f64);
    let t = std::time::Instant::now();
    for b in 0..blocks {
        let x: Vec<f32> = (0..n).map(|i| ((b * n + i) as f32 * 2.0 * std::f32::consts::PI * 220.0 / 48000.0).sin() * 0.3).collect();
        let out = p.process(&[x.clone(), x.clone()], n).expect("process");
        sum_in += x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
        sum_out += out[0].iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
    }
    let total = (blocks * n) as f64;
    println!("process {} 秒分（{} ms）。RMS 入力 {:.4} 出力 {:.4}", secs, t.elapsed().as_millis(), (sum_in / total).sqrt(), (sum_out / total).sqrt());
}
