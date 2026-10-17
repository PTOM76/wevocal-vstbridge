//! 本物の 32 ビット VST2（フリーウェア）を読み込めるかの確かめ。リポジトリにはプラグインを含めないので、
//! 環境変数 SOLIDVERB_DLL にファイルへのパスを渡したときだけ実行する（無ければ飛ばす）
use vstbridge::vst32::Vst32;

#[test]
fn load_and_name() {
    let Ok(path) = std::env::var("SOLIDVERB_DLL") else {
        eprintln!("skip: 環境変数 SOLIDVERB_DLL が無い");
        return;
    };
    let data = std::fs::read(&path).expect("read dll");
    match Vst32::load(&data) {
        Ok(mut v) => {
            let name = v.name().unwrap_or_default();
            println!("name={name:?} params={} in={} out={}", v.num_params, v.num_inputs, v.num_outputs);
            // 音を通してみる（リバーブなので数値の一致は確かめず、クラッシュせず出力が返ることを見る）
            let n = 64;
            let sine: Vec<f32> = (0..n).map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin() * 0.5).collect();
            let out = v.process(&[sine.clone(), sine.clone()], n).expect("process");
            let peak = out[0].iter().fold(0f32, |m, &x| m.max(x.abs()));
            println!("process ok: out[0].len()={} peak={peak:.4}", out[0].len());
            assert_eq!(out[0].len(), n);
            assert!(out[0].iter().all(|x| x.is_finite()), "出力に NaN/Inf が混ざっている");
        }
        Err(e) => panic!("load failed: {}", e.0),
    }
}

#[test]
fn impulse_passes_through() {
    let Ok(path) = std::env::var("SOLIDVERB_DLL") else { return; };
    let data = std::fs::read(&path).expect("read dll");
    let mut v = Vst32::load(&data).expect("load");
    // 先頭だけ 1.0 のインパルスを通す。既定のパラメーターはドライ寄りなので、素通しに近い音が返る
    let n = 256;
    let mut imp = vec![0f32; n];
    imp[0] = 1.0;
    let out = v.process(&[imp.clone(), imp.clone()], n).expect("process");
    let energy: f32 = out[0].iter().map(|x| x.abs()).sum();
    println!("出力のエネルギー= {energy:.4}（0 でなければ素通しできている）");
    assert!(out[0].iter().all(|x| x.is_finite()));
    assert!(energy > 0.0, "出力が全部 0（素通しもできていない）");
}

/// ウェット（リバーブ）の経路が実行されているかを、process が実行する命令の数で見る（調べるための表示のみ）
#[test]
#[ignore]
fn wet_path_runs() {
    let Ok(path) = std::env::var("SOLIDVERB_DLL") else { return; };
    let data = std::fs::read(&path).expect("read dll");
    let steps = |wet: f32, dry: f32| -> (u64, f32) {
        let mut v = Vst32::load(&data).expect("load");
        v.set_param(13, wet).unwrap();
        v.set_param(14, dry).unwrap();
        let n = 128;
        let mut imp = vec![0f32; n];
        imp[0] = 1.0;
        let before = v.emu.cpu.steps;
        let out = v.process(&[imp.clone(), imp.clone()], n).unwrap();
        let e: f32 = out[0].iter().map(|x| x.abs()).sum();
        (v.emu.cpu.steps - before, e)
    };
    let (s_dry, e_dry) = steps(0.0, 0.8);
    let (s_wet, e_wet) = steps(0.8, 0.0);
    println!("dry のみ: 命令 {s_dry}、energy {e_dry:.4} / wet のみ: 命令 {s_wet}、energy {e_wet:.4}");
}

/// 長く流して、最初の NaN がどこで出るか探す（VSTBRIDGE_NAN=1 で、NaN か inf になった x87 の命令が出る）。
/// （fxch の誤りを直す前は、ブロック 50 あたりでリバーブの内部が発散して NaN になっていた）
#[test]
fn long_run_no_nan() {
    let Ok(path) = std::env::var("SOLIDVERB_DLL") else { return; };
    let data = std::fs::read(&path).expect("read dll");
    let mut v = Vst32::load(&data).expect("load");
    v.set_param(13, 0.0).unwrap();
    v.set_param(14, 0.8).unwrap();
    let n = 128;
    for b in 0..80 {
        let mut imp = vec![0f32; n];
        if b == 0 {
            imp[0] = 1.0;
        }
        let out = v.process(&[imp.clone(), imp.clone()], n).unwrap();
        if let Some(i) = out[0].iter().position(|x| !x.is_finite()) {
            panic!("ブロック {b} の {i} サンプル目で NaN");
        }
    }
}

/// 環境変数 VST32_DLL で指定した 32 ビットの VST2 が、読み込めて、名前とつまみが取れて、音を通せるかを見る（調べるための表示）。
/// 例: VST32_DLL="…/SH-1 Reverb.dll" cargo test --test solidverb try_other_dll -- --ignored --nocapture
#[test]
#[ignore]
fn try_other_dll() {
    let Ok(path) = std::env::var("VST32_DLL") else { return; };
    let data = std::fs::read(&path).expect("read dll");
    let mut v = match Vst32::load(&data) {
        Ok(v) => v,
        Err(e) => panic!("load failed: {}", e.0),
    };
    let name = v.name().unwrap_or_default();
    let params: Vec<String> = (0..v.num_params).map(|i| v.param_name(i).unwrap_or_default()).collect();
    println!("name={name:?} in={} out={} params={} {params:?}", v.num_inputs, v.num_outputs, v.num_params);
    let n = 128;
    let mut imp = vec![0f32; n];
    imp[0] = 1.0;
    let out = v.process(&[imp.clone(), imp.clone()], n).expect("process");
    let e: f32 = out[0].iter().map(|x| x.abs()).sum();
    println!("process ok: energy={e:.4} finite={}", out[0].iter().all(|x| x.is_finite()));
}

/// 環境変数 VST32_DLL の VST に、インパルスを流して、ウェットの尾が出るかを見る（wet と dry のつまみの番号は WET、DRY で指定）
/// 例: VST32_DLL="…/SH-1 Reverb.dll" WET=9 DRY=10 cargo test --test solidverb try_tail -- --ignored --nocapture
#[test]
#[ignore]
fn try_tail() {
    let Ok(path) = std::env::var("VST32_DLL") else { return; };
    let get = |k: &str, d: i32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let (wet, dry) = (get("WET", 9), get("DRY", 10));
    let data = std::fs::read(&path).expect("read dll");
    let mut v = Vst32::load(&data).expect("load");
    v.set_param(wet, 0.8).unwrap();
    v.set_param(dry, 0.0).unwrap();
    let n = 128;
    let mut energy = Vec::new();
    for b in 0..60 {
        let mut imp = vec![0f32; n];
        if b == 0 {
            imp[0] = 1.0;
        }
        let out = v.process(&[imp.clone(), imp], n).unwrap();
        if let Some(i) = out[0].iter().position(|x| !x.is_finite()) {
            println!("ブロック {b} の {i} サンプル目で NaN");
            break;
        }
        energy.push(out[0].iter().map(|x| x.abs()).sum::<f32>());
    }
    let total: f32 = energy.iter().sum();
    println!("ブロックごとのエネルギー（先頭 12）: {:?}", energy.iter().take(12).map(|e| (e * 1000.0).round() / 1000.0).collect::<Vec<_>>());
    println!("合計 {total:.4}、尾（ブロック 4 以降）= {:.4}", energy.iter().skip(4).sum::<f32>());
}

/// ネイティブ（testplug/native32）と同じ条件で動かし、同じ形式で出力する（比べるため）。つまみは既定値のまま、200 ブロック
/// 例: VST32_DLL="…/SH-1 Delay.dll" cargo test --test solidverb compare_native -- --ignored --nocapture
#[test]
#[ignore]
fn compare_native() {
    let Ok(path) = std::env::var("VST32_DLL") else { return; };
    let data = std::fs::read(&path).expect("read dll");
    let mut v = Vst32::load(&data).expect("load");
    let n = 128;
    let mut energies = Vec::new();
    for b in 0..200 {
        let mut imp = vec![0f32; n];
        if b == 0 {
            imp[0] = 1.0;
        }
        let out = v.process(&[imp.clone(), imp], n).unwrap();
        if let Some(k) = out[0].iter().position(|x| !x.is_finite()) {
            println!("ブロック {b} の {k} サンプル目で NaN");
            break;
        }
        energies.push(out[0].iter().map(|x| x.abs()).sum::<f32>());
    }
    let r = |x: f32| (x * 1000.0).round() / 1000.0;
    println!("ブロックごとのエネルギー（先頭 12）: {:?}", energies.iter().take(12).map(|x| r(*x)).collect::<Vec<_>>());
    println!("合計 {:.4}、尾（ブロック 4 以降）= {:.4}、最後のブロック {:.4}", energies.iter().sum::<f32>(), energies.iter().skip(4).sum::<f32>(), energies.last().copied().unwrap_or(0.0));
}

/// 本物の CPU（testplug/native32 で、同じ DLL を 32 ビットのホストで動かしたもの）の出力と、エミュレーターの出力が一致するか。
/// 環境変数 SH1_REVERB_DLL に SH-1 Reverb.dll のパスを渡したときだけ実行する（市販ではないが再配布は不可なので、リポジトリには含めない）
#[test]
fn matches_native_reverb() {
    let Ok(path) = std::env::var("SH1_REVERB_DLL") else { return; };
    let data = std::fs::read(&path).expect("read dll");
    let mut v = Vst32::load(&data).expect("load");
    let n = 128;
    let mut energies = Vec::new();
    for b in 0..200 {
        let mut imp = vec![0f32; n];
        if b == 0 {
            imp[0] = 1.0;
        }
        let out = v.process(&[imp.clone(), imp], n).unwrap();
        assert!(out[0].iter().all(|x| x.is_finite()), "ブロック {b} で NaN");
        energies.push(out[0].iter().map(|x| x.abs()).sum::<f32>());
    }
    // 本物の CPU での値（小数 3 桁）。ブロック 0 はドライ、ブロック 9 から反射が始まる
    let want = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.14, 0.118, 0.129];
    for (i, w) in want.iter().enumerate() {
        assert!((energies[i] - w).abs() < 0.002, "ブロック {i}: エミュレーター {} 本物 {w}", energies[i]);
    }
    let total: f32 = energies.iter().sum();
    assert!((total - 17.1494).abs() < 0.01, "合計 {total}");
    assert!((energies.last().unwrap() - 0.0422).abs() < 0.002, "最後のブロック {}", energies.last().unwrap());
}
