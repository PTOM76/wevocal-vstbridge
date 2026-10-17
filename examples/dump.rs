//! 読み込んだ DLL の指定の番地から逆アセンブルする（調査用）。使い方: cargo run --example dump -- <dll> <番地(16進)> <バイト数>
use iced_x86::{Decoder, DecoderOptions, Formatter, IntelFormatter};
use vstbridge::vst32::Vst32;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let v = Vst32::load(&std::fs::read(&a[1]).unwrap()).unwrap();
    let addr = u64::from_str_radix(&a[2], 16).unwrap();
    let n: usize = a[3].parse().unwrap();
    let mut buf = vec![0u8; n];
    v.emu.mem.read(addr, &mut buf).unwrap();
    let mut dec = Decoder::with_ip(32, &buf, addr, DecoderOptions::NONE);
    let mut f = IntelFormatter::new();
    let mut s = String::new();
    while dec.can_decode() {
        let i = dec.decode();
        s.clear();
        f.format(&i, &mut s);
        println!("{:08x}  {}", i.ip(), s);
    }
}
