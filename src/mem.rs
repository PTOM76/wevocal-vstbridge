//! エミュレーターのメモリ。番地の範囲ごとに領域を割り当て、読み書きはその中だけ（外を触ったら Fault）

/// メモリを触れなかった（割り当てていない番地など）
#[derive(Debug, Clone, PartialEq)]
pub struct Fault(pub String);

pub type R<T> = Result<T, Fault>;

struct Region {
    start: u64,
    data: Vec<u8>,
}

#[derive(Default)]
pub struct Memory {
    regions: Vec<Region>,
    /// ホストが使う領域（バッファなど）の次の割り当て先
    heap_next: u64,
    /// 32 ビットのプラグイン向けの割り当て先（番地が 2^32 未満に収まる範囲）
    heap_next32: u64,
}

/// ホストが確保する領域の始まり（プラグインの DLL やスタックとぶつからない所）
pub const HEAP_BASE: u64 = 0x6000_0000_0000;
/// 32 ビットのプラグイン向け。DLL の既定の読み込み先（0x10000000 あたり）やスタック（0x7FFE0000）より低くも高くもない空き
pub const HEAP_BASE_32: u64 = 0x6000_0000;

impl Memory {
    pub fn new() -> Self {
        Memory { regions: Vec::new(), heap_next: HEAP_BASE, heap_next32: HEAP_BASE_32 }
    }

    /// start から size バイトを 0 で割り当てる
    pub fn map(&mut self, start: u64, size: usize) {
        self.regions.push(Region { start, data: vec![0; size] });
    }

    /// ホストの領域から size バイトを 16 バイト境界で確保し、その番地を返す
    pub fn alloc(&mut self, size: usize) -> u64 {
        let at = self.heap_next;
        let size = (size + 15) & !15;
        self.map(at, size.max(16));
        self.heap_next += size.max(16) as u64 + 16;
        at
    }

    /// 32 ビットのプラグインに渡せる番地（2^32 未満）で確保する
    pub fn alloc32(&mut self, size: usize) -> u32 {
        let at = self.heap_next32;
        let size = (size + 15) & !15;
        self.map(at, size.max(16));
        self.heap_next32 += size.max(16) as u64 + 16;
        at as u32
    }

    fn find(&self, addr: u64, len: usize) -> R<(usize, usize)> {
        for (i, r) in self.regions.iter().enumerate() {
            if addr >= r.start && addr + len as u64 <= r.start + r.data.len() as u64 {
                return Ok((i, (addr - r.start) as usize));
            }
        }
        Err(Fault(format!("memory {addr:#x}+{len}")))
    }

    pub fn read(&self, addr: u64, buf: &mut [u8]) -> R<()> {
        let (i, o) = self.find(addr, buf.len())?;
        buf.copy_from_slice(&self.regions[i].data[o..o + buf.len()]);
        Ok(())
    }

    pub fn write(&mut self, addr: u64, buf: &[u8]) -> R<()> {
        let (i, o) = self.find(addr, buf.len())?;
        self.regions[i].data[o..o + buf.len()].copy_from_slice(buf);
        Ok(())
    }

    /// 命令を読むための、addr から読めるだけ（最大 len）の写し
    pub fn fetch(&self, addr: u64, len: usize) -> R<&[u8]> {
        for r in &self.regions {
            if addr >= r.start && addr < r.start + r.data.len() as u64 {
                let o = (addr - r.start) as usize;
                return Ok(&r.data[o..(o + len).min(r.data.len())]);
            }
        }
        Err(Fault(format!("fetch {addr:#x}")))
    }

    /// size バイト（1、2、4、8）を小さい順（little endian）で読む
    pub fn read_n(&self, addr: u64, size: usize) -> R<u64> {
        let mut b = [0u8; 8];
        self.read(addr, &mut b[..size])?;
        Ok(u64::from_le_bytes(b))
    }

    pub fn write_n(&mut self, addr: u64, size: usize, v: u64) -> R<()> {
        self.write(addr, &v.to_le_bytes()[..size])
    }

    pub fn read_u128(&self, addr: u64) -> R<u128> {
        let mut b = [0u8; 16];
        self.read(addr, &mut b)?;
        Ok(u128::from_le_bytes(b))
    }

    pub fn write_u128(&mut self, addr: u64, v: u128) -> R<()> {
        self.write(addr, &v.to_le_bytes())
    }

    pub fn read_f32s(&self, addr: u64, n: usize) -> R<Vec<f32>> {
        let mut b = vec![0u8; n * 4];
        self.read(addr, &mut b)?;
        Ok(b.chunks(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
    }

    pub fn write_f32s(&mut self, addr: u64, v: &[f32]) -> R<()> {
        let b: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
        self.write(addr, &b)
    }

    /// 0 で終わる文字列を読む（最大 max バイト）
    pub fn read_cstr(&self, addr: u64, max: usize) -> R<String> {
        let mut out = Vec::new();
        for i in 0..max as u64 {
            let c = self.read_n(addr + i, 1)? as u8;
            if c == 0 {
                break;
            }
            out.push(c);
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}
