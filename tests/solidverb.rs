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
