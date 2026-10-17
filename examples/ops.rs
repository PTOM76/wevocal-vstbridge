//! 命令のオペランドの数と種類を表示する（iced が x87 のオペランドをどう表すかを調べる道具）
use iced_x86::{Decoder, DecoderOptions};
fn main() {
    let hex: String = std::env::args().skip(1).collect::<Vec<_>>().join("");
    let bytes: Vec<u8> = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap()).collect();
    let mut d = Decoder::with_ip(32, &bytes, 0, DecoderOptions::NONE);
    while d.can_decode() {
        let i = d.decode();
        let ops: Vec<String> = (0..i.op_count()).map(|n| format!("{:?}:{:?}", i.op_kind(n), i.op_register(n))).collect();
        println!("{}  op_count={} [{}]", i, i.op_count(), ops.join(", "));
    }
}
