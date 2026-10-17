//! インタープリター本体の 1 命令あたりのコストを測る。整数だけのループ（inc/dec/jnz）と、x87 のループ（fld/fmul/fstp）を、機械語で直接置いて回す
use std::time::Instant;
use vstbridge::emu::{Arg, Emu};

fn run(name: &str, code: &[u8], per_iter: u64, iters: u64, setup: impl Fn(&mut Emu)) {
    let mut emu = Emu::new32();
    let base = 0x1000_0000u64;
    emu.mem.map(base, 0x1000);
    emu.mem.write(base, code).unwrap();
    emu.mem.map(0x2000_0000, 0x1000);
    setup(&mut emu);
    let t = Instant::now();
    emu.call32(base as u32, &[Arg::Int(0)]).unwrap();
    let dt = t.elapsed().as_secs_f64();
    let steps = emu.cpu.steps;
    println!("{name:<28} {steps} 命令、{dt:.3} 秒 → 毎秒 {:.1} M 命令（1 命令 {:.0} ns）[{} 回 × {per_iter}]", steps as f64 / dt / 1e6, dt * 1e9 / steps as f64, iters);
}

fn main() {
    let n = 5_000_000u32;
    let n_bytes = n.to_le_bytes();
    // mov ecx, n / L: inc eax / dec ecx / jnz L / ret
    let int_loop = [0xB9, n_bytes[0], n_bytes[1], n_bytes[2], n_bytes[3], 0x40, 0x49, 0x75, 0xFC, 0xC3];
    run("整数ループ（inc dec jnz）", &int_loop, 3, n as u64, |_| {});
    // mov ecx, n / mov edx, 0x20000000 / L: fld dword [edx] / fmul dword [edx] / fstp dword [edx+4] / dec ecx / jnz L / ret
    let x87_loop = [0xB9, n_bytes[0], n_bytes[1], n_bytes[2], n_bytes[3], 0xBA, 0x00, 0x00, 0x00, 0x20, 0xD9, 0x02, 0xD8, 0x0A, 0xD9, 0x5A, 0x04, 0x49, 0x75, 0xF6, 0xC3];
    run("x87 ループ（fld fmul fstp）", &x87_loop, 5, n as u64, |e| {
        e.mem.write_f32s(0x2000_0000, &[1.5]).unwrap();
    });
    // mov ecx, n / mov edx, 0x20000000 / L: mov eax,[edx] / add eax, 1 / mov [edx+4], eax / dec ecx / jnz L / ret
    let mem_loop = [0xB9, n_bytes[0], n_bytes[1], n_bytes[2], n_bytes[3], 0xBA, 0x00, 0x00, 0x00, 0x20, 0x8B, 0x02, 0x83, 0xC0, 0x01, 0x89, 0x42, 0x04, 0x49, 0x75, 0xF5, 0xC3];
    run("メモリ読み書きループ", &mem_loop, 5, n as u64, |_| {});
}
