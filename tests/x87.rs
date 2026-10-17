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

// ---- 整数の乗除算（1 引数）。機械語を直接置いて、手計算の答えと比べる ----
fn with_regs(code: &[u8], regs: &[(usize, u64)]) -> Emu {
    run(code, |e| {
        for (i, v) in regs {
            e.cpu.regs[*i] = *v;
        }
    })
}

#[test]
fn mul_ecx_unsigned() {
    // F7 E1 = MUL ECX。eax=0x10000000、ecx=0x30 → 0x3_0000_0000 = edx:eax = 0x3:0x0
    let e = with_regs(&[0xF7, 0xE1], &[(0, 0x1000_0000), (1, 0x30)]);
    assert_eq!((e.cpu.regs[2] & 0xffff_ffff, e.cpu.regs[0] & 0xffff_ffff), (0x3, 0x0));
}

#[test]
fn imul_ecx_signed_negative() {
    // F7 E9 = IMUL ECX。eax=-3、ecx=5 → -15 = edx:eax = 0xFFFFFFFF:0xFFFFFFF1
    let e = with_regs(&[0xF7, 0xE9], &[(0, (-3i32) as u32 as u64), (1, 5)]);
    assert_eq!((e.cpu.regs[2] & 0xffff_ffff, e.cpu.regs[0] & 0xffff_ffff), (0xffff_ffff, (-15i32) as u32 as u64));
}

#[test]
fn div_ecx_unsigned() {
    // F7 F1 = DIV ECX。edx:eax = 0x1:0x00000005（= 4294967301）÷ 10 → 商 429496730（0x1999999A）、余り 1
    let e = with_regs(&[0xF7, 0xF1], &[(2, 1), (0, 5), (1, 10)]);
    assert_eq!((e.cpu.regs[0] & 0xffff_ffff, e.cpu.regs[2] & 0xffff_ffff), (429496730, 1));
}

#[test]
fn idiv_ecx_signed() {
    // F7 F9 = IDIV ECX。edx:eax = -7（edx=0xFFFFFFFF、eax=0xFFFFFFF9）÷ 2 → 商 -3、余り -1
    let e = with_regs(&[0xF7, 0xF9], &[(2, 0xffff_ffff), (0, (-7i32) as u32 as u64), (1, 2)]);
    assert_eq!(e.cpu.regs[0] & 0xffff_ffff, (-3i32) as u32 as u64);
    assert_eq!(e.cpu.regs[2] & 0xffff_ffff, (-1i32) as u32 as u64);
}

#[test]
fn mul_cl_byte() {
    // F6 E1 = MUL CL。al=200、cl=3 → 600 = ax = 0x0258
    let e = with_regs(&[0xF6, 0xE1], &[(0, 200), (1, 3)]);
    assert_eq!(e.cpu.regs[0] & 0xffff, 600);
}

// ---- 2^x を作る典型の並び（フィルターやリバーブの係数の計算に出る）----
#[test]
fn pow2_idiom() {
    // 機械語: D9 C0 (fld st0) / D9 FC (frndint) / DC E9 (fsub st1,st0) / D9 C9 (fxch) / D9 F0 (f2xm1) / D9 E8 (fld1) / DE C1 (faddp) / D9 FD (fscale) / DD D9 (fstp st1)
    // st(0)=3.5 を積んで 2^3.5 = 11.3137... を作る
    let code = [0xD9, 0xC0, 0xD9, 0xFC, 0xDC, 0xE9, 0xD9, 0xC9, 0xD9, 0xF0, 0xD9, 0xE8, 0xDE, 0xC1, 0xD9, 0xFD, 0xDD, 0xD9];
    let e = with_stack(&code, &[3.5]);
    let want = 2f64.powf(3.5);
    assert!((e.cpu.st(0) - want).abs() < 1e-9, "got {} want {}", e.cpu.st(0), want);
}

#[test]
fn pow10_via_fyl2x() {
    // 10^x = 2^(x * log2(10))。x=2 → 100。fldl2t（log2(10)を積む）→ fmul → 上の並び
    // D9 E9 (fldl2t) / DE C9 (fmulp) / 上の 2^x の並び
    let mut code = vec![0xD9, 0xE9, 0xDE, 0xC9];
    code.extend_from_slice(&[0xD9, 0xC0, 0xD9, 0xFC, 0xDC, 0xE9, 0xD9, 0xC9, 0xD9, 0xF0, 0xD9, 0xE8, 0xDE, 0xC1, 0xD9, 0xFD, 0xDD, 0xD9]);
    let e = with_stack(&code, &[2.0]);
    assert!((e.cpu.st(0) - 100.0).abs() < 1e-6, "got {}", e.cpu.st(0));
}

#[test]
fn frndint_ties_to_even_and_modes() {
    // frndint の既定は偶数丸め: 2.5 → 2、3.5 → 4
    assert_eq!(with_stack(&[0xD9, 0xFC], &[2.5]).cpu.st(0), 2.0);
    assert_eq!(with_stack(&[0xD9, 0xFC], &[3.5]).cpu.st(0), 4.0);
}

/// 並びの途中経過を 1 命令ずつ表示する（どこでずれるか探す）。cargo test --test x87 step_by_step -- --nocapture --ignored
#[test]
#[ignore]
fn step_by_step() {
    let steps: [(&str, &[u8]); 9] = [
        ("fld st0", &[0xD9, 0xC0]),
        ("frndint", &[0xD9, 0xFC]),
        ("fsub st(1),st", &[0xDC, 0xE9]),
        ("fxch", &[0xD9, 0xC9]),
        ("f2xm1", &[0xD9, 0xF0]),
        ("fld1", &[0xD9, 0xE8]),
        ("faddp", &[0xDE, 0xC1]),
        ("fscale", &[0xD9, 0xFD]),
        ("fstp st(1)", &[0xDD, 0xD9]),
    ];
    let mut code = Vec::new();
    for (i, (name, bytes)) in steps.iter().enumerate() {
        code.extend_from_slice(bytes);
        let e = with_stack(&code, &[3.5]);
        let st: Vec<String> = (0..3).map(|n| format!("{:.4}", e.cpu.st(n))).collect();
        println!("{} {:<14} → st(0..2)=[{}] 深さ={}", i + 1, name, st.join(", "), e.cpu.fpu_depth);
    }
}
