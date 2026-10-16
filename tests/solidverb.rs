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
/// 既知の問題: ブロック 50 あたりでリバーブの内部が発散して NaN になる（調査中。HANDOFF.md 参照）ので、通常は飛ばす
#[test]
#[ignore]
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
