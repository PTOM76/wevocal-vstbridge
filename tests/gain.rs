//! testplug/gain.dll を読み込んで、ネイティブのホスト（host.exe）と同じ結果になるか確かめる。
//! gain.dll が無ければ飛ばす（testplug/README の作り方でビルドする）
use vstbridge::vst::Vst;

const DLL: &str = "testplug/gain.dll";

fn load() -> Option<Vst> {
    let data = std::fs::read(DLL).ok()?;
    Some(Vst::load(&data).expect("load"))
}

#[test]
fn name_and_params() {
    let Some(mut v) = load() else {
        eprintln!("skip: {DLL} が無い");
        return;
    };
    assert_eq!(v.num_params, 1);
    assert_eq!(v.num_inputs, 2);
    assert_eq!(v.num_outputs, 2);
    assert_eq!(v.name().unwrap(), "WeVocal Test Gain");
}

#[test]
fn process_matches_native() {
    let Some(mut v) = load() else {
        return;
    };
    // host.exe と同じ: 音量 0.25、1 kHz の正弦波を 8 サンプル
    v.set_param(0, 0.25).unwrap();
    assert!((v.get_param(0).unwrap() - 0.25).abs() < 1e-6);

    let n = 8;
    let sine: Vec<f32> = (0..n).map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48000.0).sin()).collect();
    let out = v.process(&[sine.clone(), sine.clone()], n).unwrap();

    // host.exe の出力（音量 0.25 を掛けたもの）
    let want = [0.000000, 0.032632, 0.064705, 0.095671, 0.125000, 0.152190, 0.176777, 0.198338];
    for (i, (&a, &b)) in out[0].iter().zip(want.iter()).enumerate() {
        assert!((a - b).abs() < 1e-5, "sample {i}: {a} != {b}");
    }
    // 左右が同じ
    assert_eq!(out[0], out[1]);
}
