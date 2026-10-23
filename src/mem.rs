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
    /// 命令として読まれたことがあるブロックか（自己展開のコードなど。ここへの書き込みは、デコード済みの命令を無効にする）
    executed: std::cell::RefCell<Vec<bool>>,
}

/// 命令として読まれたかを覚える単位（バイト）。データの書き込みがコードの近くに当たらなければ、デコード済みの命令を捨てずに済む
const BLOCK: usize = 64;
/// ページの中の off から len バイトが触れる 64 バイトのブロックのビット
fn block_mask(off: usize, len: usize) -> u64 {
    let a = off / BLOCK;
    let b = (off + len.max(1) - 1) / BLOCK;
    let hi = if b >= 63 { u64::MAX } else { (1u64 << (b + 1)) - 1 };
    hi & !((1u64 << a) - 1)
}
const PAGE_BITS: u32 = 12;
/// alloc32 が一度に確保する領域の大きさ
const ARENA: usize = 1 << 20;
const PAGE: usize = 1 << PAGE_BITS;
/// 32 ビットの番地空間のページ数
const PAGES: usize = 1 << (32 - PAGE_BITS);

#[derive(Default)]
pub struct Memory {
    regions: Vec<Region>,
    /// ホストが使う領域（バッファなど）の次の割り当て先
    heap_next: u64,
    /// アリーナの終わり（alloc32 の領域の、使える範囲の終わり）
    arena_end: u64,
    /// 32 ビットのプラグイン向けの割り当て先（番地が 2^32 未満に収まる範囲）
    heap_next32: u64,
    /// 直前に当たった領域の添字（読み書きと命令の取得は、同じ領域に続けて当たることが多いので、まずここを調べる）
    last_data: [std::cell::Cell<usize>; 4],
    last_code: std::cell::Cell<usize>,
    /// 領域の添字を start の小さい順に並べたもの（領域が多いとき、二分探索で見つける）
    order: Vec<usize>,
    /// 4KB ページごとの、ホスト上の先頭の番地（0 なら未割り当て。番地が 2^32 未満で、ページ全体が 1 つの領域に収まるところだけ。読み書きの近道）
    pages: Vec<usize>,
    /// ページごとの、命令として読まれた 64 バイトのブロック（1 ビット 1 ブロック。近道の書き込みで、デコード済みの命令を捨てる合図に使う）
    code_pages: Vec<std::cell::Cell<u64>>,
    /// VSTBRIDGE_OOB のとき、要求サイズを見張る領域は近道に入れない
    watch_oob: bool,
    /// 命令として読まれた領域へ書き込みがあった（デコード済みの命令を捨てる合図。Emu が読んで戻す）
    pub code_dirty: bool,
}

/// ホストが確保する領域の始まり（プラグインの DLL やスタックとぶつからない所）
pub const HEAP_BASE: u64 = 0x6000_0000_0000;
/// 32 ビットのプラグイン向け。DLL の既定の読み込み先（0x10000000 あたり）やスタック（0x7FFE0000）より低くも高くもない空き
pub const HEAP_BASE_32: u64 = 0x6000_0000;

impl Memory {
    pub fn new() -> Self {
        Memory { regions: Vec::new(), heap_next: HEAP_BASE, heap_next32: HEAP_BASE_32, last_data: Default::default(), last_code: Default::default(), order: Vec::new(), arena_end: 0, pages: vec![0; PAGES], code_pages: vec![Default::default(); PAGES], watch_oob: std::env::var("VSTBRIDGE_OOB").is_ok(), code_dirty: false }
    }

    /// start から size バイトを 0 で割り当てる
    pub fn map(&mut self, start: u64, size: usize) {
        let idx = self.regions.len();
        self.regions.push(Region { start, data: vec![0; size], limit: 0, executed: Default::default() });
        let at = self.order.partition_point(|&k| self.regions[k].start < start);
        self.order.insert(at, idx);
        self.map_pages(idx);
    }

    /// ページ全体が領域に収まる 4KB ごとに、ホストの番地を表へ書く（重なるときは先に割り当てた領域を優先する）
    fn map_pages(&mut self, idx: usize) {
        let r = &self.regions[idx];
        if self.watch_oob && r.limit > 0 {
            return;
        }
        let end = r.start + r.data.len() as u64;
        if r.start >= 1 << 32 {
            return;
        }
        let first = (r.start + PAGE as u64 - 1) >> PAGE_BITS;
        let last = end.min(1 << 32) >> PAGE_BITS;
        for pg in first..last {
            let slot = &mut self.pages[pg as usize];
            if *slot == 0 {
                *slot = r.data.as_ptr() as usize + ((pg << PAGE_BITS) - r.start) as usize;
            }
        }
    }

    /// ホストの領域から size バイトを 16 バイト境界で確保し、その番地を返す
    /// （4GB 未満に置く。ページ表の近道と JIT のため）
    pub fn alloc(&mut self, size: usize) -> u64 {
        self.alloc32(size) as u64
    }

    /// 32 ビットのプラグインに渡せる番地（2^32 未満）で確保する
    pub fn alloc32(&mut self, size: usize) -> u32 {
        if !self.watch_oob {
            // 小さいブロックは、ページに揃えた大きな領域（アリーナ）から切り出す（領域が 4KB ページ全体を覆うので、読み書きの近道が使える）
            let need = ((size + 15) & !15) as u64 + 16;
            if self.heap_next32 + need > self.arena_end {
                let start = (self.heap_next32 + PAGE as u64 - 1) & !(PAGE as u64 - 1);
                let chunk = ((need as usize).max(ARENA) + PAGE - 1) & !(PAGE - 1);
                self.map(start, chunk);
                self.heap_next32 = start;
                self.arena_end = start + chunk as u64;
            }
            let at = self.heap_next32;
            self.heap_next32 += need;
            return at as u32;
        }
        let want = size;
        // 大きいブロックはページに合わせる（読み書きの近道が使える）
        let big = size >= 2 * PAGE;
        if big {
            self.heap_next32 = (self.heap_next32 + PAGE as u64 - 1) & !(PAGE as u64 - 1);
        }
        let at = self.heap_next32;
        let size = if big { (size + PAGE - 1) & !(PAGE - 1) } else { (size + 15) & !15 };
        self.map(at, size.max(16));
        // 要求サイズを覚える（範囲外の書き込みを見つけるため）
        if let Some(r) = self.regions.last_mut() {
            r.limit = want;
        }
        self.heap_next32 += size.max(16) as u64 + 16;
        at as u32
    }

    /// addr を含みうる領域（start が addr 以下で一番大きいもの）の添字
    fn lookup(&self, addr: u64) -> Option<usize> {
        let p = self.order.partition_point(|&k| self.regions[k].start <= addr);
        if p == 0 { None } else { Some(self.order[p - 1]) }
    }

    fn find(&self, addr: u64, len: usize) -> R<(usize, usize)> {
        let hit = |r: &Region| addr >= r.start && addr + len as u64 <= r.start + r.data.len() as u64;
        // 直前に当たった領域を先に調べる（多くの読み書きはここで済む。スタック、DLL のデータ、バッファが交互に来るので 4 つ覚える）
        for k in 0..4 {
            let last = self.last_data[k].get();
            if let Some(r) = self.regions.get(last) {
                if hit(r) {
                    if k > 0 {
                        self.last_data[k].set(self.last_data[0].get());
                        self.last_data[0].set(last);
                    }
                    return Ok((last, (addr - r.start) as usize));
                }
            }
        }
        let p = self.order.partition_point(|&k| self.regions[k].start <= addr);
        for &i in self.order[p.saturating_sub(6)..p].iter().rev() {
            if hit(&self.regions[i]) {
                self.last_data[3].set(self.last_data[2].get());
                self.last_data[2].set(self.last_data[1].get());
                self.last_data[1].set(self.last_data[0].get());
                self.last_data[0].set(i);
                return Ok((i, (addr - self.regions[i].start) as usize));
            }
        }
        // 領域が重なっている場合（大きな領域の中に別の領域がある）は、二分探索で見つからないので全部調べる
        for (i, r) in self.regions.iter().enumerate() {
            if hit(r) {
                self.last_data[3].set(self.last_data[2].get());
                self.last_data[2].set(self.last_data[1].get());
                self.last_data[1].set(self.last_data[0].get());
                self.last_data[0].set(i);
                return Ok((i, (addr - r.start) as usize));
            }
        }
        Err(Fault(format!("memory {addr:#x}+{len}")))
    }

    pub fn read(&self, addr: u64, buf: &mut [u8]) -> R<()> {
        let off = (addr & (PAGE as u64 - 1)) as usize;
        if addr >> 32 == 0 && off + buf.len() <= PAGE {
            // SAFETY: addr < 2^32 なので添字は pages の長さ未満
            let p = unsafe { *self.pages.get_unchecked((addr >> PAGE_BITS) as usize) };
            if p != 0 {
                // SAFETY: p は領域のデータ（Vec は動かない）の中のページの先頭で、off + len はページに収まる
                unsafe { std::ptr::copy_nonoverlapping((p + off) as *const u8, buf.as_mut_ptr(), buf.len()) };
                return Ok(());
            }
        }
        let (i, o) = self.find(addr, buf.len())?;
        buf.copy_from_slice(&self.regions[i].data[o..o + buf.len()]);
        Ok(())
    }

    pub fn write(&mut self, addr: u64, buf: &[u8]) -> R<()> {
        let off = (addr & (PAGE as u64 - 1)) as usize;
        if addr >> 32 == 0 && off + buf.len() <= PAGE {
            let pg = (addr >> PAGE_BITS) as usize;
            let p = self.pages[pg];
            if p != 0 {
                if self.code_pages[pg].get() & block_mask(off, buf.len()) != 0 {
                    self.code_dirty = true;
                }
                // SAFETY: read と同じ
                unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), (p + off) as *mut u8, buf.len()) };
                return Ok(());
            }
        }
        let (i, o) = self.find(addr, buf.len())?;
        // 確保したブロックの要求サイズを超えた書き込みを報告する（VSTBRIDGE_OOB=1。調査用）
        {
            let ex = self.regions[i].executed.borrow();
            if !ex.is_empty() && !buf.is_empty() {
                let last = ((o + buf.len() - 1) / BLOCK).min(ex.len() - 1);
                if (o / BLOCK..=last).any(|b| b < ex.len() && ex[b]) {
                    self.code_dirty = true;
                }
            }
        }
        let lim = self.regions[i].limit;
        if lim > 0 && o + buf.len() > lim && std::env::var("VSTBRIDGE_OOB").is_ok() {
            eprintln!("[oob] 確保 {} バイトのブロック {:#x} の外へ書いた: オフセット {} から {} バイト", lim, self.regions[i].start, o, buf.len());
        }
        self.regions[i].data[o..o + buf.len()].copy_from_slice(buf);
        Ok(())
    }

    /// ページ表（4KB ごとのホストの番地）の先頭。JIT が生成コードに埋める（割り当て直さないので寿命の間は変わらない）
    pub fn pages_ptr(&self) -> u32 {
        self.pages.as_ptr() as usize as u32
    }

    /// code_pages の先頭（ホストの番地。JIT が、命令のあるページへの書き込みを見分けるのに使う）
    pub fn code_pages_ptr(&self) -> u32 {
        self.code_pages.as_ptr() as usize as u32
    }

    /// 命令を読むための、addr から読めるだけ（最大 len）の写し
    pub fn fetch(&self, addr: u64, len: usize) -> R<&[u8]> {
        if addr >> 32 == 0 {
            let first = (addr >> PAGE_BITS) as usize;
            let off = (addr & (PAGE as u64 - 1)) as usize;
            let c = &self.code_pages[first];
            c.set(c.get() | block_mask(off, len.min(PAGE - off)));
            if off + len > PAGE && first + 1 < PAGES {
                let c = &self.code_pages[first + 1];
                c.set(c.get() | block_mask(0, off + len - PAGE));
            }
        }
        fn slice(r: &Region, addr: u64, len: usize) -> &[u8] {
            let o = (addr - r.start) as usize;
            let mut ex = r.executed.borrow_mut();
            if ex.is_empty() {
                ex.resize(r.data.len() / BLOCK + 1, false);
            }
            let end = (o + len).min(r.data.len());
            for b in o / BLOCK..=(end.saturating_sub(1)) / BLOCK {
                ex[b] = true;
            }
            &r.data[o..(o + len).min(r.data.len())]
        }
        let last = self.last_code.get();
        if let Some(r) = self.regions.get(last) {
            if addr >= r.start && addr < r.start + r.data.len() as u64 {
                return Ok(slice(r, addr, len));
            }
        }
        if let Some(i) = self.lookup(addr) {
            let r = &self.regions[i];
            if addr < r.start + r.data.len() as u64 {
                self.last_code.set(i);
                return Ok(slice(r, addr, len));
            }
        }
        for (i, r) in self.regions.iter().enumerate() {
            if addr >= r.start && addr < r.start + r.data.len() as u64 {
                self.last_code.set(i);
                return Ok(slice(r, addr, len));
            }
        }
        Err(Fault(format!("fetch {addr:#x}")))
    }

    /// size バイト（1、2、4、8）を小さい順（little endian）で読む
    /// 4 バイトを読む（4KB ページの近道つき）
    #[inline(always)]
    pub fn read_u32(&self, addr: u64) -> R<u32> {
        let off = (addr & (PAGE as u64 - 1)) as usize;
        if addr >> 32 == 0 && off <= PAGE - 4 {
            // SAFETY: addr < 2^32 なので添字は pages の長さ未満
            let p = unsafe { *self.pages.get_unchecked((addr >> PAGE_BITS) as usize) };
            if p != 0 {
                // SAFETY: p + off は領域のデータの中（ページ全体が領域に収まっている）で、4 バイト読める
                return Ok(unsafe { ((p + off) as *const u32).read_unaligned() });
            }
        }
        self.read_n(addr, 4).map(|v| v as u32)
    }

    #[inline(always)]
    pub fn read_u64(&self, addr: u64) -> R<u64> {
        let off = (addr & (PAGE as u64 - 1)) as usize;
        if addr >> 32 == 0 && off <= PAGE - 8 {
            // SAFETY: addr < 2^32 なので添字は pages の長さ未満
            let p = unsafe { *self.pages.get_unchecked((addr >> PAGE_BITS) as usize) };
            if p != 0 {
                // SAFETY: read_u32 と同じ
                return Ok(unsafe { ((p + off) as *const u64).read_unaligned() });
            }
        }
        self.read_n(addr, 8)
    }

    #[inline(always)]
    pub fn write_u32(&mut self, addr: u64, v: u32) -> R<()> {
        let off = (addr & (PAGE as u64 - 1)) as usize;
        if addr >> 32 == 0 && off <= PAGE - 4 {
            let pg = (addr >> PAGE_BITS) as usize;
            let p = self.pages[pg];
            if p != 0 {
                if self.code_pages[pg].get() & block_mask(off, 4) != 0 {
                    self.code_dirty = true;
                }
                // SAFETY: read_u32 と同じ
                unsafe { ((p + off) as *mut u32).write_unaligned(v) };
                return Ok(());
            }
        }
        self.write_n(addr, 4, v as u64)
    }

    #[inline(always)]
    pub fn write_u64(&mut self, addr: u64, v: u64) -> R<()> {
        let off = (addr & (PAGE as u64 - 1)) as usize;
        if addr >> 32 == 0 && off <= PAGE - 8 {
            let pg = (addr >> PAGE_BITS) as usize;
            let p = self.pages[pg];
            if p != 0 {
                if self.code_pages[pg].get() & block_mask(off, 8) != 0 {
                    self.code_dirty = true;
                }
                // SAFETY: read_u32 と同じ
                unsafe { ((p + off) as *mut u64).write_unaligned(v) };
                return Ok(());
            }
        }
        self.write_n(addr, 8, v)
    }

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
