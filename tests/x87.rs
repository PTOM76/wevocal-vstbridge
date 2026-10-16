//! x87 の命令の結果を、手計算した答えと比べる。機械語を直接置いて動かす（32 ビット）
use vstbridge::emu::{Arg, Emu};

/// コードを置いて、関数として呼ぶ。コードの最後に ret（0xC3）を付ける
fn run(code: &[u8], setup: impl FnOnce(&mut Emu)) -> Emu {
    let mut emu = Emu::new32();
    let base = 0x1000_0000u64;
    emu.mem.map(base, 0x1000);
    let mut bytes = code.to_vec();
    bytes.push(0xC3);
    emu.mem.write(base, &bytes).unwrap();
    setup(&mut emu);
    emu.call32(base as u32, &[Arg::Int(0)]).unwrap();
    emu
}

/// st(0) に 10、st(1) に 3 ... のように積んでから命令を実行する。fld1 で 1 を積んで fadd で足して作る代わりに、cpu に直接積む
fn with_stack(code: &[u8], values: &[f64]) -> Emu {
    run(code, |e| {
        for v in values {
            e.cpu.fpush(*v);
        }
    })
}

#[test]
fn fsubp_is_sti_minus_st0() {
    // DE E9 = FSUBP ST(1), ST(0)。積む順は 10、3（st(0)=3, st(1)=10）→ st(1) = st(1) - st(0) = 7、pop して st(0)=7
    let e = with_stack(&[0xDE, 0xE9], &[10.0, 3.0]);
    assert_eq!(e.cpu.st(0), 7.0);
}

#[test]
fn fsubrp_is_st0_minus_sti() {
    // DE E1 = FSUBRP ST(1), ST(0)。st(1) = st(0) - st(1) = 3 - 10 = -7
    let e = with_stack(&[0xDE, 0xE1], &[10.0, 3.0]);
    assert_eq!(e.cpu.st(0), -7.0);
}

#[test]
fn fdivp_is_sti_over_st0() {
    // DE F9 = FDIVP ST(1), ST(0)。st(1) = st(1) / st(0) = 10 / 4
    let e = with_stack(&[0xDE, 0xF9], &[10.0, 4.0]);
    assert_eq!(e.cpu.st(0), 2.5);
}

#[test]
fn fdivrp_is_st0_over_sti() {
    // DE F1 = FDIVRP ST(1), ST(0)。st(1) = st(0) / st(1) = 4 / 10
    let e = with_stack(&[0xDE, 0xF1], &[10.0, 4.0]);
    assert!((e.cpu.st(0) - 0.4).abs() < 1e-12);
}

#[test]
fn fsub_st0_sti_and_sti_st0() {
    // D8 E1 = FSUB ST(0), ST(1)：st(0) = st(0) - st(1) = 3 - 10 = -7
    let e = with_stack(&[0xD8, 0xE1], &[10.0, 3.0]);
    assert_eq!(e.cpu.st(0), -7.0);
    // DC E9 = FSUB ST(1), ST(0)：st(1) = st(1) - st(0)。Intel のマニュアルでは DC E8+i が FSUB ST(i), ST(0)
    let e = with_stack(&[0xDC, 0xE9], &[10.0, 3.0]);
    assert_eq!(e.cpu.st(1), 7.0);
}

#[test]
fn fscale_and_f2xm1() {
    // fscale は st(0) * 2^trunc(st(1))。st(0)=1.5（スケールされる値）、st(1)=3（指数）→ 1.5 * 8 = 12。積む順は st(1) が先
    let e = with_stack(&[0xD9, 0xFD], &[3.0, 1.5]);
    assert_eq!(e.cpu.st(0), 12.0);
    // f2xm1: 2^0.5 - 1
    let e = with_stack(&[0xD9, 0xF0], &[0.5]);
    assert!((e.cpu.st(0) - (2f64.powf(0.5) - 1.0)).abs() < 1e-12);
}

#[test]
fn fyl2x_pops_and_multiplies() {
    // st(0)=8（x）、st(1)=2（y）→ y * log2(x) = 2 * 3 = 6、pop して st(0)=6
    let e = with_stack(&[0xD9, 0xF1], &[2.0, 8.0]);
    assert_eq!(e.cpu.st(0), 6.0);
}
