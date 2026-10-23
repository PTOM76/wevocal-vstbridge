//! 64 ビットの VST2 を読み込んで、testplug/host.exe（本物の CPU）と同じ入力（param0=0.25、1 kHz の正弦波 8 サンプル）を通す（調査用）。使い方: cargo run --release --example probe -- <dll>
use vstbridge::vst::Vst;
fn main() {
    let path = std::env::args().nth(1).expect("dll");
    let mut v = match Vst::load(&std::fs::read(&path).unwrap()) {
        Ok(v) => v,
        Err(e) => return println!("load: {}", e.0),
    };
    println!("name: {:?} params={} in={} out={}", v.name().map_err(|e| e.0), v.num_params, v.num_inputs, v.num_outputs);
    let _ = v.set_param(0, 0.25);
    println!("param0={:?}", v.get_param(0).map_err(|e| e.0));
    let s: Vec<f32> = (0..8).map(|i| (2.0 * 3.14159265f32 * 1000.0 * i as f32 / 48000.0).sin()).collect();
    match v.process(&[s.clone(), s], 8) {
        Ok(o) => println!("{}", o[0].iter().map(|x| format!("{x:.6}")).collect::<Vec<_>>().join(" ")),
        Err(e) => println!("process: {}", e.0),
    }
    // 1 秒分を流して速さを測る（VSTBRIDGE_PROF=1 なら命令ごとの内訳も）
    let t = std::time::Instant::now();
    let steps = v.emu.cpu.steps;
    for b in 0..375 {
        let x: Vec<f32> = (0..128).map(|i| (((b * 128 + i) as f32) * 0.0576).sin() * 0.5).collect();
        if let Err(e) = v.process(&[x.clone(), x], 128) {
            return println!("process: {}", e.0);
        }
    }
    println!("1 秒分: {:.3} 秒、{} M 命令", t.elapsed().as_secs_f64(), (v.emu.cpu.steps - steps) / 1_000_000);
    if let Some(p) = &v.emu.prof {
        let mut rows: Vec<_> = p.iter().collect();
        rows.sort_by_key(|(_, (_, t))| std::cmp::Reverse(*t));
        for (m, (c, t)) in rows.iter().take(15) {
            let name: iced_x86::Mnemonic = unsafe { std::mem::transmute(**m as u16) };
            println!("{:<12} {c:>10} 回 {:>8.1} Mサイクル {:>5.0}/回", format!("{name:?}"), *t as f64 / 1e6, *t as f64 / *c as f64);
        }
    }
}
