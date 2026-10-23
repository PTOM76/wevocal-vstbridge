//! testplug/sse.dll（SSE の型の変換、並べ替え、movd、movq を使う）を、ネイティブのホスト（host.exe）と同じ結果になるか確かめる。
//! 範囲外の cvttss2si（0x80000000）も通る。sse.dll が無ければ飛ばす（testplug/README の作り方でビルドする）
use vstbridge::vst::Vst;

#[test]
fn process_matches_native() {
    let Ok(data) = std::fs::read("testplug/sse.dll") else {
        eprintln!("skip: testplug/sse.dll が無い");
        return;
    };
    let mut v = Vst::load(&data).expect("load");
    assert_eq!(v.name().unwrap(), "WeVocal Test SSE");
    // host.exe と同じ: つまみ 0.25、1 kHz の正弦波を 8 サンプル
    v.set_param(0, 0.25).unwrap();
    let n = 8;
    let sine: Vec<f32> = (0..n).map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48000.0).sin()).collect();
    let out = v.process(&[sine.clone(), sine], n).unwrap();
    // host.exe sse.dll の出力
    let want = [0.000000, 0.102372, 0.201351, 0.302828, 0.401570, 0.495426, 0.582771, 0.661798];
    for (i, (&a, &b)) in out[0].iter().zip(want.iter()).enumerate() {
        assert!((a - b).abs() < 1e-5, "sample {i}: {a} != {b}");
    }
}
