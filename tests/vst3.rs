// VST3 を読み込み、クラスの一覧を取り、音を通す（環境変数 VST3_DLL に 1 ファイルの .vst3 のパスを渡したときだけ。市販品や配布物はリポジトリに入れない）
use vstbridge::vst3::Vst3;
use vstbridge::vst3_process::Vst3Plugin;

#[test]
fn vst3_load_and_process() {
    let Ok(path) = std::env::var("VST3_DLL") else {
        eprintln!("skip: VST3_DLL が無い");
        return;
    };
    let dll = std::fs::read(&path).expect("read");
    let v = Vst3::load(&dll).expect("load");
    assert!(v.classes.iter().any(|c| c.category == "Audio Module Class"), "音を処理するクラスが無い");
    let mut p = Vst3Plugin::open(&dll, 48000.0).expect("open");
    assert_eq!(p.outputs, 2);
    let n = 128;
    let mut energy = 0f64;
    for b in 0..100 {
        let x: Vec<f32> = (0..n).map(|i| ((b * n + i) as f32 * 0.03).sin() * 0.3).collect();
        let out = p.process(&[x.clone(), x], n).expect("process");
        assert!(out.iter().all(|c| c.iter().all(|v| v.is_finite())), "出力に NaN や inf");
        energy += out[0].iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
    }
    assert!(energy > 1e-6, "出力が無音");
}
