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
        }
        Err(e) => panic!("load failed: {}", e.0),
    }
}
