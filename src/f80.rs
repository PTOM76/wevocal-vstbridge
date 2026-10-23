//! x87 の 80 ビットの浮動小数点（符号 1、指数 15、整数のビットを含む仮数 64）と f64 の変換。x87 のレジスタは f64 で持つので、読み書きのときに変える
/// 10 バイト（小さい順）を f64 に（最も近い値に丸める）
pub fn to_f64(b: [u8; 10]) -> f64 {
    let mant = u64::from_le_bytes(b[..8].try_into().unwrap());
    let se = u16::from_le_bytes([b[8], b[9]]);
    let neg = se & 0x8000 != 0;
    let exp = (se & 0x7fff) as i32;
    let v = if exp == 0x7fff {
        if mant << 1 == 0 {
            f64::INFINITY
        } else {
            // NaN: 仮数の上位を残し、静かな NaN にする
            f64::from_bits(0x7ff8_0000_0000_0000 | ((mant << 1) >> 12))
        }
    } else if mant == 0 {
        0.0
    } else {
        // 仮数を f64 に丸めてから、2 の冪を掛ける（指数 0 は非正規化数で、指数 1 と同じ倍率）
        ldexp(mant as f64, exp.max(1) - 16383 - 63)
    };
    if neg { -v } else { v }
}

/// f64 を 10 バイト（小さい順）に（f64 は 80 ビットに必ず収まる）
pub fn from_f64(v: f64) -> [u8; 10] {
    let bits = v.to_bits();
    let sign = ((bits >> 63) as u16) << 15;
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1 << 52) - 1);
    let (e, mant) = if exp == 0x7ff {
        (0x7fff, (1 << 63) | (frac << 11))
    } else if exp == 0 {
        if frac == 0 {
            (0, 0)
        } else {
            // 非正規化数: 先頭の 1 を最上位のビットへ寄せる
            let z = frac.leading_zeros() as i32;
            (1 - 1023 + 16383 - (z - 11), frac << z)
        }
    } else {
        (exp - 1023 + 16383, (1 << 63) | (frac << 11))
    };
    let mut b = [0u8; 10];
    b[..8].copy_from_slice(&mant.to_le_bytes());
    b[8..].copy_from_slice(&(sign | e as u16).to_le_bytes());
    b
}

/// x × 2^n（途中であふれたり 0 になったりしないよう、何回かに分けて掛ける）
fn ldexp(mut x: f64, mut n: i32) -> f64 {
    while n > 1000 {
        x *= f64::from_bits(((1000 + 1023) as u64) << 52);
        n -= 1000;
    }
    while n < -1000 {
        x *= f64::from_bits(((-1000 + 1023) as u64) << 52);
        n += 1000;
        if x == 0.0 {
            return x;
        }
    }
    x * f64::from_bits(((n + 1023) as u64) << 52)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip() {
        for v in [0.0, -0.0, 1.0, -2.5, 1e300, -1e-300, 5e-324, f64::MAX, f64::MIN_POSITIVE, f64::INFINITY, f64::NEG_INFINITY, std::f64::consts::PI] {
            assert_eq!(to_f64(from_f64(v)).to_bits(), v.to_bits(), "{v}");
        }
        assert!(to_f64(from_f64(f64::NAN)).is_nan());
        // 1.0 は指数 16383、仮数 1<<63
        assert_eq!(from_f64(1.0), [0, 0, 0, 0, 0, 0, 0, 0x80, 0xff, 0x3f]);
        // f64 の範囲を超える値は無限大、下回る値は 0
        assert_eq!(to_f64([0, 0, 0, 0, 0, 0, 0, 0x80, 0xfe, 0x7f]), f64::INFINITY);
        assert_eq!(to_f64([0, 0, 0, 0, 0, 0, 0, 0x80, 0x01, 0x00]), 0.0);
    }
}
