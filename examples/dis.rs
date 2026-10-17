//! 機械語を逆アセンブルして表示する（テストの機械語が意図した命令かを確かめる道具）。使い方: cargo run --example dis -- D9C0 D9FC ...（16 進、2 桁ずつ）
use iced_x86::{Decoder, DecoderOptions};
fn main() {
    let hex: String = std::env::args().skip(1).collect::<Vec<_>>().join("");
    let bytes: Vec<u8> = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap()).collect();
    let mut d = Decoder::with_ip(32, &bytes, 0, DecoderOptions::NONE);
    while d.can_decode() {
        let i = d.decode();
        println!("{:04x}: {}", i.ip(), i);
    }
}
