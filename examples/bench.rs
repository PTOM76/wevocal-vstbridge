//! エミュレーターの速さを測る。32 ビットの VST2 に、指定した秒数のインパルス/無音を 128 サンプルずつ流し、かかった時間と命令数を出す。
//! 使い方: cargo run --release --example bench -- <dll> [秒数=2]
use std::time::Instant;
use vstbridge::vst32::Vst32;

fn main() {
    let path = std::env::args().nth(1).expect("DLL のパス");
    let secs: f64 = std::env::args().nth(2).and_then(|v| v.parse().ok()).unwrap_or(2.0);
    let data = std::fs::read(&path).expect("read");
    let t0 = Instant::now();
    let mut v = Vst32::load(&data).expect("load");
    let load = t0.elapsed();
    let load_steps = v.emu.cpu.steps;
    let n = 128;
    let blocks = (secs * 48000.0 / n as f64) as usize;
    let t1 = Instant::now();
    for b in 0..blocks {
        let mut imp = vec![0f32; n];
        if b == 0 {
            imp[0] = 1.0;
        }
        v.process(&[imp.clone(), imp], n).unwrap();
    }
    let run = t1.elapsed();
    let steps = v.emu.cpu.steps - load_steps;
    println!("読み込み {:.2} 秒（{} 命令）", load.as_secs_f64(), load_steps);
    if let Some(p) = &v.emu.prof {
        let mut rows: Vec<_> = p.iter().collect();
        rows.sort_by_key(|(_, (_, t))| std::cmp::Reverse(*t));
        for (m, (c, t)) in rows.iter().take(25) {
            let name: iced_x86::Mnemonic = unsafe { std::mem::transmute(**m as u16) };
            println!("{:<12} {c:>10} 回 {:>10.1} Mサイクル {:>6.0} サイクル/回", format!("{name:?}"), *t as f64 / 1e6, *t as f64 / *c as f64);
        }
    }
    println!("{secs} 秒分の処理: {:.3} 秒（実時間の {:.2} 倍）、{} 命令、1 秒あたり {:.1} M 命令", run.as_secs_f64(), run.as_secs_f64() / secs, steps, steps as f64 / run.as_secs_f64() / 1e6);
}

// 命令ごとの内訳（VSTBRIDGE_PROF=1）
