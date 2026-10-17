//! エミュレーターのメモリ。番地の範囲ごとに領域を割り当て、読み書きはその中だけ（外を触ったら Fault）

/// メモリを触れなかった（割り当てていない番地など）
#[derive(Debug, Clone, PartialEq)]
pub struct Fault(pub String);

pub type R<T> = Result<T, Fault>;

struct Region {
    start: u64,
    data: Vec<u8>,
    /// ホストが確保した領域で、プラグインが要求したサイズ（これを超える書き込みは範囲外。0 なら見張らない）
    limit: usize,
    /// 命令として読まれたことがある領域か（自己展開のコードなど。ここへの書き込みは、デコード済みの命令を無効にする）
    executed: std::cell::Cell<bool>,
}

#[derive(Default)]
pub struct Memory {
    regions: Vec<Region>,
    /// ホストが使う領域（バッファなど）の次の割り当て先
    heap_next: u64,
    /// 32 ビットのプラグイン向けの割り当て先（番地が 2^32 未満に収まる範囲）
    heap_next32: u64,
    /// 直前に当たった領域の添字（読み書きと命令の取得は、同じ領域に続けて当たることが多いので、まずここを調べる）
    last_data: std::cell::Cell<usize>,
    last_code: std::cell::Cell<usize>,
    /// 命令として読まれた領域へ書き込みがあった（デコード済みの命令を捨てる合図。Emu が読んで戻す）
    pub code_dirty: bool,
}

/// ホストが確保する領域の始まり（プラグインの DLL やスタックとぶつからない所）
pub const HEAP_BASE: u64 = 0x6000_0000_0000;
/// 32 ビットのプラグイン向け。DLL の既定の読み込み先（0x10000000 あたり）やスタック（0x7FFE0000）より低くも高くもない空き
pub const HEAP_BASE_32: u64 = 0x6000_0000;

impl Memory {
    pub fn new() -> Self {
        Memory { regions: Vec::new(), heap_next: HEAP_BASE, heap_next32: HEAP_BASE_32, last_data: Default::default(), last_code: Default::default(), code_dirty: false }
    }

    /// start から size バイトを 0 で割り当てる
    pub fn map(&mut self, start: u64, size: usize) {
        self.regions.push(Region { start, data: vec![0; size], limit: 0, executed: Default::default() });
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
        let want = size;
        let size = (size + 15) & !15;
        self.map(at, size.max(16));
        // 要求サイズを覚える（範囲外の書き込みを見つけるため）
        if let Some(r) = self.regions.last_mut() {
            r.limit = want;
        }
        self.heap_next32 += size.max(16) as u64 + 16;
        at as u32
    }

    fn find(&self, addr: u64, len: usize) -> R<(usize, usize)> {
        let hit = |r: &Region| addr >= r.start && addr + len as u64 <= r.start + r.data.len() as u64;
        // 直前に当たった領域を先に調べる（多くの読み書きはここで済む）
        let last = self.last_data.get();
        if let Some(r) = self.regions.get(last) {
            if hit(r) {
                return Ok((last, (addr - r.start) as usize));
            }
        }
        for (i, r) in self.regions.iter().enumerate() {
            if hit(r) {
                self.last_data.set(i);
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
        // 確保したブロックの要求サイズを超えた書き込みを報告する（VSTBRIDGE_OOB=1。調査用）
        if self.regions[i].executed.get() {
            self.code_dirty = true;
        }
        let lim = self.regions[i].limit;
        if lim > 0 && o + buf.len() > lim && std::env::var("VSTBRIDGE_OOB").is_ok() {
            eprintln!("[oob] 確保 {} バイトのブロック {:#x} の外へ書いた: オフセット {} から {} バイト", lim, self.regions[i].start, o, buf.len());
        }
        self.regions[i].data[o..o + buf.len()].copy_from_slice(buf);
        Ok(())
    }

    /// 命令を読むための、addr から読めるだけ（最大 len）の写し
    pub fn fetch(&self, addr: u64, len: usize) -> R<&[u8]> {
        fn slice(r: &Region, addr: u64, len: usize) -> &[u8] {
            let o = (addr - r.start) as usize;
            &r.data[o..(o + len).min(r.data.len())]
        }
        let last = self.last_code.get();
        if let Some(r) = self.regions.get(last) {
            if addr >= r.start && addr < r.start + r.data.len() as u64 {
                r.executed.set(true);
                return Ok(slice(r, addr, len));
            }
        }
        for (i, r) in self.regions.iter().enumerate() {
            if addr >= r.start && addr < r.start + r.data.len() as u64 {
                self.last_code.set(i);
                r.executed.set(true);
                return Ok(slice(r, addr, len));
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
