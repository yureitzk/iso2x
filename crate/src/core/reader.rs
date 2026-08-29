use crate::utils::u64_to_js_number;
use js_sys::{Function, Number, Uint8Array};
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom};
use std::mem;
use wasm_bindgen::prelude::*;

/// Cache block size for the scattered-read path. Sized at 64 KiB to balance
/// metadata overhead without fetching unused megabytes.
pub(crate) const DEFAULT_BLOCK_SIZE: usize = 64 * 1024;

/// Default readahead window for sequential bulk reads (8 MiB).
/// Minimizes WASM/JS bridge round trips while keeping memory usage modest.
pub(crate) const DEFAULT_SEQ_WINDOW: usize = 8 * 1024 * 1024;

/// Maximum sequential readahead window size (256 MiB) to prevent
/// unbounded memory usage from caller-supplied window sizes.
const MAX_SEQ_WINDOW: usize = 256 * 1024 * 1024;

/// Absolute byte budget limit for streak-based speculative prefetching
/// in Cached mode. Hardcoded in bytes to scale independently of block size.
const MAX_STREAK_PREFETCH_BYTES: u64 = 2 * 1024 * 1024;
const STREAK_THRESHOLD: u32 = 3;

/// Maximum resident blocks in Cached mode (512 * 64 KiB = 32 MiB). A circuit
/// breaker against malformed/looping disc images.
const MAX_RESIDENT_BLOCKS: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReadMode {
    /// Small-block sparse cache. Best for scattered, revisited reads.
    Cached,
    /// Single large readahead window. Best for read-once linear scans.
    Sequential,
}

pub(crate) struct JsReader {
    pub(crate) read_fn: Function,
    pub(crate) position: u64,
    pub(crate) size: u64,
    block_size: usize,
    base_offset: u64,
    mode: ReadMode,
    // Sparse chunk cache keyed by block index.
    blocks: HashMap<u64, Vec<u8>>,
    // Last-touched tick per resident block; eviction frees the coldest first.
    last_touched: HashMap<u64, u64>,
    // Bumped once per ensure_range call, driving last_touched.
    touch_clock: u64,
    // End block of the last run, for streak-based speculative widening.
    last_run_end_block: Option<u64>,
    seq_streak: u32,
    seq_window_size: usize,
    seq_buffer: Vec<u8>,
    seq_buffer_start: u64,
    // Total number of read_fn round trips made by this reader.
    fetch_count: u32,
    // Cached mode's reused, grow-only destination for call_read_fn -
    // avoids allocating fresh on every coalesced block fetch. Sequential
    // mode writes straight into `seq_buffer` instead, so this is unused
    // (and released) while in that mode.
    read_scratch: Vec<u8>,
}

impl JsReader {
    pub(crate) fn new(read_fn: Function, size: u64) -> Self {
        Self::new_with_base_offset(read_fn, size, 0)
    }

    /// Creates a reader starting at `base_offset` for sub-range reads.
    pub(crate) fn new_with_base_offset(read_fn: Function, size: u64, base_offset: u64) -> Self {
        JsReader {
            read_fn,
            position: 0,
            size,
            block_size: DEFAULT_BLOCK_SIZE,
            base_offset,
            mode: ReadMode::Cached,
            blocks: HashMap::new(),
            last_touched: HashMap::new(),
            touch_clock: 0,
            last_run_end_block: None,
            seq_streak: 0,
            seq_window_size: DEFAULT_SEQ_WINDOW,
            seq_buffer: Vec::new(),
            seq_buffer_start: 0,
            fetch_count: 0,
            read_scratch: Vec::new(),
        }
    }

    /// Switches between scattered block caching and sequential bulk reads.
    /// Switching modes drops the unused mode's buffered state to free memory.
    pub(crate) fn set_sequential_mode(&mut self, enabled: bool) {
        let new_mode = if enabled {
            ReadMode::Sequential
        } else {
            ReadMode::Cached
        };
        if new_mode == self.mode {
            return;
        }
        self.mode = new_mode;
        match new_mode {
            ReadMode::Sequential => {
                self.blocks.clear();
                self.last_touched.clear();
                self.last_run_end_block = None;
                self.seq_streak = 0;
                // Cached mode's staging buffer - Sequential mode has no
                // use for it, so release it rather than let it sit
                // retained at Cached mode's high-water mark.
                self.read_scratch = Vec::new();
            }
            ReadMode::Cached => {
                self.seq_buffer.clear();
                self.seq_buffer_start = 0;
            }
        }
    }

    /// Overrides the sequential readahead window size, clamped to `MAX_SEQ_WINDOW`.
    pub(crate) fn set_sequential_window(&mut self, bytes: usize) {
        self.seq_window_size = bytes.clamp(1, MAX_SEQ_WINDOW);
    }

    /// Executes the JS `read_fn`, validates the returned payload, and
    /// writes it into `out[..n]`. Returns `n`. Grow-only: `out` may be
    /// longer than `n` afterward if it already had more capacity than
    /// this call needed - callers must slice to `..n` (or truncate) if
    /// they need `out`'s length to reflect exactly `n`.
    ///
    /// Takes a caller-supplied `out` rather than a fixed field so each
    /// mode can write straight into its own real destination - Sequential
    /// mode's `seq_buffer`, Cached mode's `read_scratch` staging buffer -
    /// instead of always landing in one shared buffer and needing a
    /// second one to move it into place.
    fn call_read_fn(
        &mut self,
        abs_pos: u64,
        to_fetch: usize,
        out: &mut Vec<u8>,
    ) -> io::Result<usize> {
        // Catches an offset/length that can't round-trip through a JS `number` exactly.
        let js_offset = u64_to_js_number(self.base_offset + abs_pos, "read offset")
            .map_err(|e| io::Error::other(format!("{e:#}")))?;
        let to_fetch_f64 = u64_to_js_number(to_fetch as u64, "read length")
            .map_err(|e| io::Error::other(format!("{e:#}")))?;
        // Return type stays `JsValue` (not `Uint8Array`) so the `instanceof`
        // check in `dyn_into` below still runs on a misbehaving `readFn`.
        let read_fn: &Function<fn(Number, Number) -> JsValue> = self.read_fn.unchecked_ref();
        let result = read_fn
            .call2(
                &JsValue::NULL,
                &Number::from(js_offset),
                &Number::from(to_fetch_f64),
            )
            .map_err(|e| io::Error::other(format!("{e:?}")))?;
        let array = result
            .dyn_into::<Uint8Array>()
            .map_err(|_| io::Error::other("read_fn did not return Uint8Array"))?;
        let n = array.byte_length() as usize;
        if n > to_fetch {
            return Err(io::Error::other(format!(
                "readFn returned {n} bytes but only {to_fetch} were requested"
            )));
        }
        // A short read is only legitimate at the declared end of the source;
        // otherwise treat it as a failed fetch rather than caching it as EOF.
        if n < to_fetch {
            let reached = abs_pos.saturating_add(n as u64);
            if reached < self.size {
                return Err(io::Error::other(format!(
                    "readFn returned {n} of {to_fetch} requested bytes at offset {abs_pos}, \
                     but {} bytes remain before declared size {} - treating as a failed \
                     fetch rather than EOF",
                    self.size - reached,
                    self.size
                )));
            }
        }
        // Only zero-fills the newly grown tail (if any) - bytes already
        // within the old length are about to be overwritten by copy_to
        // below, so there's nothing to gain from re-zeroing them.
        if out.len() < n {
            out.resize(n, 0);
        }
        // Deliberate, not a missed optimization: writing straight into a
        // `Uint8Array::view` over `out` would drop this copy, but that
        // view aliases wasm linear memory with no lifetime tied to `out`,
        // so it's unsound the moment `readFn` retains it past this call or
        // wasm memory grows before it's used. `wasm-streams`' BYOB
        // `read_with_buffer` hits the same JS/wasm boundary and reaches
        // the same conclusion: reuse a plain buffer, copy once, no unsafe.
        array.copy_to(&mut out[..n]);
        self.fetch_count += 1;
        Ok(n)
    }

    /// Ensures all blocks covering [`abs_pos`, `abs_pos` + len) are resident,
    /// coalescing runs of missing blocks into single `read_fn` fetches.
    fn ensure_range(&mut self, abs_pos: u64, len: usize) -> io::Result<()> {
        if len == 0 || abs_pos >= self.size {
            return Ok(());
        }
        let block_size_u64 = self.block_size as u64;
        let start_block = abs_pos / block_size_u64;
        let end_abs = abs_pos.saturating_add(len as u64).min(self.size);
        let end_block = (end_abs.saturating_sub(1)) / block_size_u64;

        // Protect blocks already resident in [start_block, end_block] so a
        // later fetch in this loop can't evict one this call still needs.
        let mut fetched_this_call: HashSet<u64> = HashSet::new();
        {
            let mut b = start_block;
            while b <= end_block {
                if self.blocks.contains_key(&b) {
                    fetched_this_call.insert(b);
                }
                b += 1;
            }
        }
        let mut b = start_block;
        while b <= end_block {
            if self.blocks.contains_key(&b) {
                b += 1;
                continue;
            }
            let mut run_end = b;
            while run_end < end_block && !self.blocks.contains_key(&(run_end + 1)) {
                run_end += 1;
            }
            let is_contiguous_with_last = self.last_run_end_block == Some(b.wrapping_sub(1));
            self.seq_streak = if is_contiguous_with_last {
                self.seq_streak + 1
            } else {
                1
            };
            let mut fetch_last = run_end;
            if self.seq_streak >= STREAK_THRESHOLD {
                let max_extra_blocks = (MAX_STREAK_PREFETCH_BYTES / block_size_u64).max(1);
                let extra = max_extra_blocks.min(
                    self.size
                        .saturating_sub((run_end + 1) * block_size_u64)
                        .div_ceil(block_size_u64),
                );
                let mut widened = run_end;
                let cap = run_end + extra;
                while widened < cap && !self.blocks.contains_key(&(widened + 1)) {
                    let next_start = (widened + 1) * block_size_u64;
                    if next_start >= self.size {
                        break;
                    }
                    widened += 1;
                }
                fetch_last = widened;
            }
            self.fetch_block_range(b, fetch_last, &mut fetched_this_call)?;
            self.last_run_end_block = Some(fetch_last);
            b = run_end + 1;
        }
        // Stamp every block this call touched with a fresh tick.
        self.touch_clock += 1;
        let tick = self.touch_clock;
        for &k in &fetched_this_call {
            self.last_touched.insert(k, tick);
        }
        Ok(())
    }

    /// Fetches [`first_block`, `last_block`] inclusive in a single round trip.
    /// `protect` shields blocks this `ensure_range` call still needs from eviction.
    fn fetch_block_range(
        &mut self,
        first_block: u64,
        last_block: u64,
        protect: &mut HashSet<u64>,
    ) -> io::Result<()> {
        let block_size_u64 = self.block_size as u64;
        let range_start = first_block * block_size_u64;
        let range_end_exclusive = ((last_block + 1) * block_size_u64).min(self.size);
        let incoming_blocks = usize::try_from(last_block - first_block + 1)
            .map_err(|_| io::Error::other("block range does not fit in usize"))?;
        if self.blocks.len() + incoming_blocks > MAX_RESIDENT_BLOCKS {
            let need_to_free =
                (self.blocks.len() + incoming_blocks).saturating_sub(MAX_RESIDENT_BLOCKS);
            // Evict the coldest `need_to_free` non-protected blocks; ties
            // break on block index for determinism.
            let mut candidates: Vec<u64> = self
                .blocks
                .keys()
                .filter(|k| !protect.contains(k))
                .copied()
                .collect();
            candidates.sort_by_key(|k| (self.last_touched.get(k).copied().unwrap_or(0), *k));
            let freed = need_to_free.min(candidates.len());
            for &k in candidates.iter().take(freed) {
                self.blocks.remove(&k);
                self.last_touched.remove(&k);
            }
            self.last_run_end_block = None;
            self.seq_streak = 0;
        }
        if range_start >= range_end_exclusive {
            for b in first_block..=last_block {
                self.blocks.entry(b).or_default();
                protect.insert(b);
            }
            return Ok(());
        }
        let to_fetch = usize::try_from(range_end_exclusive - range_start)
            .map_err(|_| io::Error::other("range does not fit in usize"))?;

        // Route `read_scratch` through a local so `call_read_fn` can take
        // `&mut self` and the destination buffer separately without a
        // double-mutable-borrow on `self`.
        let mut scratch = mem::take(&mut self.read_scratch);
        let result = self.call_read_fn(range_start, to_fetch, &mut scratch);
        self.read_scratch = scratch;
        let n = result?;

        for b in first_block..=last_block {
            let start_in_range = usize::try_from(b - first_block)
                .expect("bounded by incoming_blocks, already checked above to fit usize")
                * self.block_size;
            if start_in_range >= n {
                self.blocks.entry(b).or_default();
                protect.insert(b);
                continue;
            }
            let end_in_range = (start_in_range + self.block_size).min(n);
            // Each cached block needs its own owned Vec to outlive this
            // call (keyed independently in `self.blocks`), so this
            // .to_vec() is unavoidable - `read_scratch` itself is just the
            // reused source buffer this slices from.
            let block_bytes = self.read_scratch[start_in_range..end_in_range].to_vec();
            self.blocks.entry(b).or_insert(block_bytes);
            protect.insert(b);
        }
        Ok(())
    }

    fn read_cached(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.position < self.size {
            // Bounded by buf.len() before the cast, so this can't
            // truncate even on 32-bit WASM targets.
            let remaining = self.size - self.position;
            let want = usize::try_from(remaining.min(buf.len() as u64))
                .expect("bounded by buf.len() above");
            if want > 0 {
                self.ensure_range(self.position, want)?;
            }
        }

        let mut total = 0usize;
        while total < buf.len() && self.position < self.size {
            let block_size_u64 = self.block_size as u64;
            let block_index = self.position / block_size_u64;
            // Always < block_size_u64, which fits in usize by definition.
            let offset_in_block = usize::try_from(self.position % block_size_u64)
                .expect("modulo of block_size_u64 always fits in usize");

            let Some(block) = self.blocks.get(&block_index) else {
                break;
            };

            if offset_in_block >= block.len() {
                break;
            }
            let n = (buf.len() - total).min(block.len() - offset_in_block);
            buf[total..total + n].copy_from_slice(&block[offset_in_block..offset_in_block + n]);
            total += n;
            self.position += n as u64;
        }
        Ok(total)
    }

    fn seq_buffer_covers(&self, abs_pos: u64) -> bool {
        !self.seq_buffer.is_empty()
            && abs_pos >= self.seq_buffer_start
            && abs_pos < self.seq_buffer_start + self.seq_buffer.len() as u64
    }

    fn fill_sequential_window(&mut self, abs_pos: u64) -> io::Result<()> {
        let remaining = self.size.saturating_sub(abs_pos);
        let to_fetch = usize::try_from(remaining.min(self.seq_window_size as u64))
            .map_err(|_| io::Error::other("window size does not fit in usize"))?;
        if to_fetch == 0 {
            self.seq_buffer.clear();
            self.seq_buffer_start = abs_pos;
            return Ok(());
        }
        // Writes straight into `seq_buffer` via the same take/put-back
        // pattern `fetch_block_range` uses - it's already the buffer this
        // window needs to fill, so this stays alloc-free at steady state
        // without retaining a `read_scratch` this mode never uses.
        let mut buf = mem::take(&mut self.seq_buffer);
        let result = self.call_read_fn(abs_pos, to_fetch, &mut buf);
        self.seq_buffer = buf;
        let n = result?;
        self.seq_buffer.truncate(n);
        self.seq_buffer_start = abs_pos;
        Ok(())
    }

    fn read_sequential(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Bounded by buf.len() before the cast, as in read_cached.
        let remaining = self.size - self.position;
        let avail_total =
            usize::try_from(remaining.min(buf.len() as u64)).expect("bounded by buf.len() above");
        let want = buf.len().min(avail_total);
        if want == 0 {
            return Ok(0);
        }
        if !self.seq_buffer_covers(self.position) {
            self.fill_sequential_window(self.position)?;
        }
        if self.seq_buffer.is_empty() {
            return Ok(0);
        }
        // seq_buffer_covers()/fill_sequential_window() guarantee position
        // is within [seq_buffer_start, seq_buffer_start + seq_buffer.len()),
        // so this is always < seq_buffer.len(), which fits in usize.
        let offset_in_buf = usize::try_from(self.position - self.seq_buffer_start)
            .expect("bounded by seq_buffer.len() above");
        let avail_in_window = self.seq_buffer.len() - offset_in_buf;
        let n = want.min(avail_in_window);
        buf[..n].copy_from_slice(&self.seq_buffer[offset_in_buf..offset_in_buf + n]);
        self.position += n as u64;
        Ok(n)
    }
}

impl Read for JsReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.position >= self.size {
            return Ok(0);
        }
        match self.mode {
            ReadMode::Sequential => self.read_sequential(buf),
            ReadMode::Cached => self.read_cached(buf),
        }
    }
}

impl Seek for JsReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let too_large = || io::Error::new(io::ErrorKind::InvalidInput, "seek offset too large");
        let overflow = || io::Error::new(io::ErrorKind::InvalidInput, "seek overflow");
        let new_pos: i64 = match pos {
            SeekFrom::Start(n) => i64::try_from(n).map_err(|_| too_large())?,
            SeekFrom::End(n) => {
                let size = i64::try_from(self.size).map_err(|_| too_large())?;
                size.checked_add(n).ok_or_else(overflow)?
            }
            SeekFrom::Current(n) => {
                let cur = i64::try_from(self.position).map_err(|_| too_large())?;
                cur.checked_add(n).ok_or_else(overflow)?
            }
        };
        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }
        self.position = u64::try_from(new_pos).expect("new_pos is non-negative");
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    /// Mocks a JS read function returning short reads at genuine EOF.
    fn make_reader(size: u64, fill_byte: u8) -> JsReader {
        let closure = wasm_bindgen::closure::Closure::wrap(Box::new(
            move |offset: f64, length: f64| -> Uint8Array {
                let offset = offset as u64;
                let length = length as u64;
                let avail = size.saturating_sub(offset).min(length);
                let avail = usize::try_from(avail).unwrap_or(0);
                let data = vec![fill_byte; avail];
                Uint8Array::from(data.as_slice())
            },
        )
            as Box<dyn FnMut(f64, f64) -> Uint8Array>);
        let read_fn: Function = closure.as_ref().clone().unchecked_into();
        closure.forget();
        JsReader::new(read_fn, size)
    }

    #[wasm_bindgen_test]
    fn remaining_bytes_exactly_2_32_does_not_truncate_to_zero_read_cached() {
        let size: u64 = (1u64 << 32) + 65536;
        let mut reader = make_reader(size, 0xCD);
        reader.set_sequential_mode(false);
        reader.seek(SeekFrom::Start(65536)).unwrap();
        assert_eq!(size - reader.position, 1u64 << 32);
        let mut buf = [0u8; 2048];
        reader
            .read_exact(&mut buf)
            .expect("must not report EOF when 4GiB+ of real data remains (Cached mode)");
        assert!(buf.iter().all(|&b| b == 0xCD));
    }

    #[wasm_bindgen_test]
    fn remaining_bytes_exactly_2_32_does_not_truncate_to_zero_read_sequential() {
        let size: u64 = (1u64 << 32) + 65536;
        let mut reader = make_reader(size, 0xCD);
        reader.set_sequential_mode(true);
        reader.seek(SeekFrom::Start(65536)).unwrap();
        assert_eq!(size - reader.position, 1u64 << 32);
        let mut buf = [0u8; 2048];
        reader
            .read_exact(&mut buf)
            .expect("must not report EOF when 4GiB+ of real data remains (Sequential mode)");
        assert!(buf.iter().all(|&b| b == 0xCD));
    }

    #[wasm_bindgen_test]
    fn reads_still_stop_at_genuine_eof() {
        let size: u64 = 4096;
        let mut reader = make_reader(size, 0xAB);
        reader.seek(SeekFrom::Start(4090)).unwrap();
        let mut buf = [0u8; 100];
        let err = reader.read_exact(&mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[wasm_bindgen_test]
    fn resident_block_survives_eviction_triggered_later_in_same_ensure_range_call() {
        let size: u64 = 40_000_000;
        let mut reader = make_reader(size, 0x42);
        reader.set_sequential_mode(false);
        let block_size = DEFAULT_BLOCK_SIZE as u64;
        let resident_block = MAX_RESIDENT_BLOCKS as u64 - 3;
        for i in 0..=resident_block {
            reader.blocks.insert(i, vec![0x42u8; DEFAULT_BLOCK_SIZE]);
        }
        assert_eq!(reader.blocks.len(), MAX_RESIDENT_BLOCKS - 2);
        // Prime the streak so fetching the next block widens.
        let first_missing_block = resident_block + 1;
        assert!(
            !reader.blocks.contains_key(&first_missing_block),
            "test setup bug: first_missing_block should not be seeded"
        );
        reader.last_run_end_block = Some(resident_block);
        reader.seq_streak = STREAK_THRESHOLD;
        let abs_pos = resident_block * block_size;
        let len = (2 * block_size) as usize;
        reader.ensure_range(abs_pos, len).unwrap();
        assert!(
            !reader.blocks.contains_key(&0),
            "expected eviction to have fired during this call (block 0 should have been swept)"
        );
        assert!(
            reader.blocks.contains_key(&resident_block),
            "already-resident block was evicted by a fetch its own ensure_range call triggered"
        );
        reader.seek(SeekFrom::Start(abs_pos)).unwrap();
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).expect(
            "must not report a short read / MISS for a block this call guaranteed resident",
        );
        assert!(buf.iter().all(|&b| b == 0x42));
    }

    struct Xorshift64(u64);
    impl Xorshift64 {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        /// Uniform-enough (not cryptographic) value in [0, bound).
        fn next_range(&mut self, bound: u64) -> u64 {
            if bound == 0 {
                0
            } else {
                self.next_u64() % bound
            }
        }
    }

    fn make_pattern_reader(size: u64) -> JsReader {
        let closure = wasm_bindgen::closure::Closure::wrap(Box::new(
            move |offset: f64, length: f64| -> Uint8Array {
                let offset = offset as u64;
                let length = length as u64;
                let avail = size.saturating_sub(offset).min(length);
                let avail = usize::try_from(avail).unwrap_or(0);
                let data: Vec<u8> = (0..avail as u64)
                    .map(|i| ((offset + i) % 256) as u8)
                    .collect();
                Uint8Array::from(data.as_slice())
            },
        )
            as Box<dyn FnMut(f64, f64) -> Uint8Array>);
        let read_fn: Function = closure.as_ref().clone().unchecked_into();
        closure.forget();
        JsReader::new(read_fn, size)
    }

    fn read_from_cache(reader: &JsReader, abs_pos: u64, len: usize) -> Vec<u8> {
        let block_size = DEFAULT_BLOCK_SIZE as u64;
        let mut out = Vec::with_capacity(len);
        let mut pos = abs_pos;
        let end = abs_pos + len as u64;
        while pos < end {
            let block_index = pos / block_size;
            let offset_in_block = (pos % block_size) as usize;
            let block = reader
                .blocks
                .get(&block_index)
                .expect("caller must verify residency before calling read_from_cache");
            let avail = block.len().saturating_sub(offset_in_block);
            let take = avail.min((end - pos) as usize);
            out.extend_from_slice(&block[offset_in_block..offset_in_block + take]);
            pos += take as u64;
            if take == 0 {
                break;
            }
        }
        out
    }

    #[wasm_bindgen_test]
    fn eviction_never_frees_a_block_touched_more_recently_than_one_it_keeps() {
        let size: u64 = 48 * 1024 * 1024;
        let block_size = DEFAULT_BLOCK_SIZE as u64;
        let mut reader = make_pattern_reader(size);
        reader.set_sequential_mode(false);
        let mut rng = Xorshift64(0x1eaf_c0de_f00d_1234);
        const ITERATIONS: usize = 500;
        let mut shadow_touch: HashMap<u64, u64> = HashMap::new();
        for call_idx in 0..ITERATIONS {
            let len: usize = if rng.next_range(10) == 0 {
                1 + (rng.next_range(4 * block_size) as usize)
            } else {
                1 + (rng.next_range(2 * block_size) as usize)
            };
            let abs_pos = rng.next_range(size.saturating_sub(1));
            let resident_before: HashSet<u64> = reader.blocks.keys().copied().collect();
            reader.ensure_range(abs_pos, len).unwrap_or_else(|e| {
                panic!("call {call_idx}: ensure_range({abs_pos}, {len}) failed: {e}")
            });
            let current_tick = reader.touch_clock;
            let evicted: Vec<u64> = resident_before
                .iter()
                .filter(|k| !reader.blocks.contains_key(k))
                .copied()
                .collect();
            if !evicted.is_empty() {
                let coldest_surviving_tick = reader
                    .blocks
                    .keys()
                    .filter_map(|k| {
                        let t = reader.last_touched.get(k).copied().unwrap_or(0);
                        (t < current_tick).then_some(t)
                    })
                    .min();
                if let Some(coldest_surviving_tick) = coldest_surviving_tick {
                    for &k in &evicted {
                        let evicted_tick = shadow_touch.get(&k).copied().unwrap_or(0);
                        assert!(
                            evicted_tick <= coldest_surviving_tick,
                            "call {call_idx}: block {k} (last touched tick {evicted_tick}) was \
                         evicted while a colder-or-equal block survived (tick \
                         {coldest_surviving_tick}) - LRU invariant violated: eviction must \
                         always free the coldest eligible block(s) first"
                        );
                    }
                }
            }
            for (&k, &t) in reader.last_touched.iter() {
                shadow_touch.insert(k, t);
            }
        }
    }

    #[wasm_bindgen_test]
    fn short_read_far_from_declared_size_is_reported_as_an_io_error_not_eof() {
        let size: u64 = 1_000_000;
        let real_fill_byte = 0xAAu8;
        let closure = wasm_bindgen::closure::Closure::wrap(Box::new(
            move |_offset: f64, length: f64| -> Uint8Array {
                let avail = (length as u64).min(10);
                let data = vec![real_fill_byte; avail as usize];
                Uint8Array::from(data.as_slice())
            },
        )
            as Box<dyn FnMut(f64, f64) -> Uint8Array>);
        let read_fn: Function = closure.as_ref().clone().unchecked_into();
        closure.forget();
        let mut reader = JsReader::new(read_fn, size);
        reader.set_sequential_mode(false);
        let mut buf = [0u8; 1024];
        let err = reader.read_exact(&mut buf).unwrap_err();
        assert_ne!(
            err.kind(),
            io::ErrorKind::UnexpectedEof,
            "a short read 999,990 bytes away from declared EOF must not be \
             reported as ordinary end-of-file"
        );
        assert!(
            !reader.blocks.contains_key(&0),
            "a failed/truncated fetch must not leave a poisoned partial block cached - \
             that would silently block any future retry at this offset"
        );
    }

    #[wasm_bindgen_test]
    fn genuine_short_read_at_non_block_aligned_eof_still_succeeds() {
        let size: u64 = DEFAULT_BLOCK_SIZE as u64 + 777; // EOF lands mid-block
        let mut reader = make_reader(size, 0x99);
        reader.set_sequential_mode(false);
        reader.seek(SeekFrom::Start(size - 10)).unwrap();
        let mut buf = [0u8; 10];
        reader
            .read_exact(&mut buf)
            .expect("a short read that lands exactly on declared EOF must still succeed");
        assert!(buf.iter().all(|&b| b == 0x99));
    }

    #[wasm_bindgen_test]
    fn ensure_range_invariant_holds_under_randomized_calls_near_eviction_cap() {
        let size: u64 = 48 * 1024 * 1024;
        let block_size = DEFAULT_BLOCK_SIZE as u64;
        let mut reader = make_pattern_reader(size);
        reader.set_sequential_mode(false);
        let mut rng = Xorshift64(0x5eed_1234_dead_beef);
        const ITERATIONS: usize = 300;
        for iter in 0..ITERATIONS {
            let len: usize = if rng.next_range(10) == 0 {
                1 + (rng.next_range(4 * block_size) as usize)
            } else {
                1 + (rng.next_range(2 * block_size) as usize)
            };
            let abs_pos = rng.next_range(size.saturating_sub(1));
            reader.ensure_range(abs_pos, len).unwrap_or_else(|e| {
                panic!("iter {iter}: ensure_range({abs_pos}, {len}) failed: {e}")
            });
            let end_abs = abs_pos.saturating_add(len as u64).min(size);
            let start_block = abs_pos / block_size;
            let end_block = (end_abs.saturating_sub(1)) / block_size;
            // Residency: every block this call covers must be resident.
            let mut b = start_block;
            while b <= end_block {
                assert!(
                    reader.blocks.contains_key(&b),
                    "iter {iter}: block {b} missing after ensure_range({abs_pos}, {len}) \
                     returned Ok (range [{start_block}..={end_block}], resident_count={})",
                    reader.blocks.len()
                );
                b += 1;
            }
            let max_single_call_blocks =
                (end_block - start_block + 1) + MAX_STREAK_PREFETCH_BYTES / block_size + 1;
            assert!(
                reader.blocks.len() as u64 <= MAX_RESIDENT_BLOCKS as u64 + max_single_call_blocks,
                "iter {iter}: resident block count {} grew unboundedly past MAX_RESIDENT_BLOCKS \
                 ({MAX_RESIDENT_BLOCKS})",
                reader.blocks.len()
            );
            // Content correctness: bytes read back must match the pattern.
            let content_len = (end_abs - abs_pos) as usize;
            let got = read_from_cache(&reader, abs_pos, content_len);
            for (i, &byte) in got.iter().enumerate() {
                let expected = ((abs_pos + i as u64) % 256) as u8;
                assert_eq!(
                    byte,
                    expected,
                    "iter {iter}: content mismatch at abs offset {} (ensure_range({abs_pos}, {len}))",
                    abs_pos + i as u64
                );
            }
        }
    }

    fn run_read_oracle(mut reader: JsReader, size: u64, seed: u64) {
        let reference: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let mut rng = Xorshift64(seed);
        const ITERATIONS: usize = 300;
        let mut position: u64 = 0;
        for iter in 0..ITERATIONS {
            if rng.next_range(4) == 0 {
                position = rng.next_range(size);
                reader.seek(SeekFrom::Start(position)).unwrap();
            }
            let want_len = 1 + rng.next_range(3 * DEFAULT_BLOCK_SIZE as u64) as usize;
            let mut buf = vec![0u8; want_len];
            let mut got = 0usize;
            loop {
                let n = reader.read(&mut buf[got..]).unwrap_or_else(|e| {
                    panic!("iter {iter}: read() at position {position} failed: {e}")
                });
                if n == 0 {
                    break;
                }
                got += n;
                if got == buf.len() {
                    break;
                }
            }
            let expected_len = (size - position).min(want_len as u64) as usize;
            assert_eq!(
                got, expected_len,
                "iter {iter}: read {got} bytes from position {position}, expected {expected_len} \
                 (requested {want_len}, size {size})"
            );
            assert_eq!(
                &buf[..got],
                &reference[position as usize..position as usize + got],
                "iter {iter}: content mismatch reading {got} bytes from position {position}"
            );
            position += got as u64;
            if position >= size {
                position = 0;
                reader.seek(SeekFrom::Start(0)).unwrap();
            }
        }
    }

    #[wasm_bindgen_test]
    fn read_matches_reference_slice_under_randomized_seeks_cached_mode() {
        let size: u64 = 48 * 1024 * 1024;
        let mut reader = make_pattern_reader(size);
        reader.set_sequential_mode(false);
        run_read_oracle(reader, size, 0xbeef_1234_dead_5eed);
    }

    #[wasm_bindgen_test]
    fn read_matches_reference_slice_under_randomized_seeks_sequential_mode() {
        let size: u64 = 48 * 1024 * 1024;
        let mut reader = make_pattern_reader(size);
        reader.set_sequential_mode(true);
        run_read_oracle(reader, size, 0xf00d_5678_cafe_9999);
    }

    #[wasm_bindgen_test]
    fn hot_region_survives_repeated_scans_under_lru_eviction() {
        use std::cell::Cell;
        use std::rc::Rc;
        let size: u64 = 64 * 1024 * 1024;
        let block_size = DEFAULT_BLOCK_SIZE as u64;
        let hot_start = 0u64;
        let hot_len = (4 * block_size) as usize;
        let hot_end = hot_start + hot_len as u64;
        let hot_fetch_count = Rc::new(Cell::new(0u32));
        let hot_fetch_count_cb = hot_fetch_count.clone();
        let closure = wasm_bindgen::closure::Closure::wrap(Box::new(
            move |offset: f64, length: f64| -> Uint8Array {
                let offset = offset as u64;
                let length = length as u64;
                if offset < hot_end && offset + length > hot_start {
                    hot_fetch_count_cb.set(hot_fetch_count_cb.get() + 1);
                }
                let avail = size.saturating_sub(offset).min(length);
                let avail = usize::try_from(avail).unwrap_or(0);
                let data: Vec<u8> = (0..avail as u64)
                    .map(|i| ((offset + i) % 256) as u8)
                    .collect();
                Uint8Array::from(data.as_slice())
            },
        )
            as Box<dyn FnMut(f64, f64) -> Uint8Array>);
        let read_fn: Function = closure.as_ref().clone().unchecked_into();
        closure.forget();
        let mut reader = JsReader::new(read_fn, size);
        reader.set_sequential_mode(false);
        let chunk_blocks: u64 = 64; // 4 MiB per call
        let chunk_len = (chunk_blocks * block_size) as usize;
        let scan_blocks: u64 = MAX_RESIDENT_BLOCKS as u64 + 100;
        const REVISITS: u64 = 5;
        for i in 0..REVISITS {
            let scan_start = hot_end + i * block_size;
            let scan_end = (scan_start + scan_blocks * block_size).min(size);
            let mut pos = scan_start;
            while pos < scan_end {
                reader.ensure_range(hot_start, hot_len).unwrap();
                let len = chunk_len.min((scan_end - pos) as usize);
                if len == 0 {
                    break;
                }
                reader.ensure_range(pos, len).unwrap();
                pos += chunk_len as u64;
            }
        }
        let hot_start_block = hot_start / block_size;
        let hot_end_block = (hot_end - 1) / block_size;
        let hot_still_resident =
            (hot_start_block..=hot_end_block).all(|b| reader.blocks.contains_key(&b));
        assert!(
            hot_still_resident,
            "hot region should survive repeated scans under LRU-by-tick eviction"
        );
        assert_eq!(
            hot_fetch_count.get(),
            1,
            "hot region should be fetched from read_fn exactly once across {REVISITS} \
             revisits, got {} - it's being evicted and refetched",
            hot_fetch_count.get()
        );
    }

    #[wasm_bindgen_test]
    fn sequential_read_crosses_window_edge_within_single_read_exact_call() {
        let size: u64 = 100_000;
        let mut reader = make_pattern_reader(size);
        reader.set_sequential_mode(true);
        reader.set_sequential_window(4096); // deliberately small vs. the read below
        let reference: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let start_pos: u64 = 777; // not window-aligned, on purpose
        reader.seek(SeekFrom::Start(start_pos)).unwrap();
        let want_len = 20_000usize; // spans several 4096-byte windows
        let mut buf = vec![0u8; want_len];
        reader
            .read_exact(&mut buf)
            .expect("read spanning multiple sequential windows must succeed");
        assert_eq!(
            buf.as_slice(),
            &reference[start_pos as usize..start_pos as usize + want_len],
            "content mismatch reading across sequential window edges"
        );
        assert!(
            reader.fetch_count > 1,
            "test is vacuous unless it actually forced more than one window fill, got {} fetches",
            reader.fetch_count
        );
    }

    #[wasm_bindgen_test]
    fn sequential_mode_toggle_mid_stream_clears_state_and_stays_correct() {
        let size: u64 = 5_000_000;
        let mut reader = make_pattern_reader(size);
        let reference: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        // Start Cached, read a chunk, confirm the cache actually populated.
        let mut buf1 = vec![0u8; 10_000];
        reader.read_exact(&mut buf1).unwrap();
        assert_eq!(&buf1[..], &reference[0..10_000]);
        assert!(
            !reader.blocks.is_empty(),
            "test setup bug: cached-mode read should have populated blocks"
        );
        // Toggle to Sequential mid-stream: cache state must be dropped.
        reader.set_sequential_mode(true);
        assert!(
            reader.blocks.is_empty(),
            "switching to Sequential mode must clear the block cache"
        );
        assert!(
            reader.last_touched.is_empty(),
            "switching to Sequential mode must clear last_touched"
        );
        let mut buf2 = vec![0u8; 20_000];
        reader.read_exact(&mut buf2).unwrap();
        assert_eq!(&buf2[..], &reference[10_000..30_000]);
        assert!(
            !reader.seq_buffer.is_empty(),
            "test setup bug: sequential-mode read should have populated seq_buffer"
        );
        // Toggle back to Cached mid-stream: seq buffer state must be dropped.
        reader.set_sequential_mode(false);
        assert!(
            reader.seq_buffer.is_empty(),
            "switching to Cached mode must clear the sequential buffer"
        );
        assert_eq!(
            reader.seq_buffer_start, 0,
            "switching to Cached mode must reset seq_buffer_start"
        );
        // Continue again from position 30_000, now back in Cached mode.
        let mut buf3 = vec![0u8; 10_000];
        reader.read_exact(&mut buf3).unwrap();
        assert_eq!(&buf3[..], &reference[30_000..40_000]);
        assert!(
            !reader.blocks.is_empty(),
            "cached-mode read after toggling back should repopulate the block cache"
        );
    }

    #[wasm_bindgen_test]
    fn sequential_window_edge_crossing_combined_with_2_32_boundary() {
        let size: u64 = (1u64 << 32) + 65536;
        let mut reader = make_pattern_reader(size);
        reader.set_sequential_mode(true);
        reader.set_sequential_window(4096);
        let start_pos: u64 = (1u64 << 32) - 6000;
        reader.seek(SeekFrom::Start(start_pos)).unwrap();
        let want_len = 12_000usize; // crosses 2^32 and several window edges
        let mut buf = vec![0u8; want_len];
        reader
            .read_exact(&mut buf)
            .expect("read spanning the 2^32 boundary across multiple windows must succeed");
        for (i, &byte) in buf.iter().enumerate() {
            let expected = ((start_pos + i as u64) % 256) as u8;
            assert_eq!(
                byte,
                expected,
                "content mismatch at offset {} (near 2^32 boundary, window edges)",
                start_pos + i as u64
            );
        }
        assert!(
            reader.fetch_count > 1,
            "test is vacuous unless it actually forced more than one window fill, got {} fetches",
            reader.fetch_count
        );
    }

    #[wasm_bindgen_test]
    fn seek_start_beyond_i64_max_is_rejected() {
        let mut reader = make_reader(4096, 0);
        let err = reader.seek(SeekFrom::Start(u64::MAX)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[wasm_bindgen_test]
    fn seek_end_with_negative_offset_lands_at_correct_position() {
        let size: u64 = 4096;
        let mut reader = make_reader(size, 0);
        let pos = reader.seek(SeekFrom::End(-100)).unwrap();
        assert_eq!(pos, size - 100);
    }

    #[wasm_bindgen_test]
    fn seek_end_zero_lands_exactly_at_size() {
        let size: u64 = 4096;
        let mut reader = make_reader(size, 0);
        let pos = reader.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(pos, size);
        let mut buf = [0u8; 1];
        assert_eq!(reader.read(&mut buf).unwrap(), 0);
    }

    #[wasm_bindgen_test]
    fn seek_end_overflow_is_rejected() {
        let mut reader = make_reader(4096, 0);
        let err = reader.seek(SeekFrom::End(i64::MAX)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[wasm_bindgen_test]
    fn seek_current_overflow_is_rejected() {
        let mut reader = make_reader(4096, 0);
        reader.seek(SeekFrom::Start(1000)).unwrap();
        let err = reader.seek(SeekFrom::Current(i64::MAX)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[wasm_bindgen_test]
    fn seek_before_start_is_rejected() {
        let size: u64 = 4096;
        let mut reader = make_reader(size, 0);
        let err = reader.seek(SeekFrom::End(-(size as i64) - 1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        reader.seek(SeekFrom::Start(50)).unwrap();
        let err = reader.seek(SeekFrom::Current(-51)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[wasm_bindgen_test]
    fn seek_past_eof_succeeds_and_subsequent_read_reports_eof() {
        let size: u64 = 4096;
        let mut reader = make_reader(size, 0xEE);
        let pos = reader.seek(SeekFrom::Start(size + 1000)).unwrap();
        assert_eq!(pos, size + 1000);
        let mut buf = [0u8; 10];
        assert_eq!(reader.read(&mut buf).unwrap(), 0);
    }

    #[wasm_bindgen_test]
    fn rejected_seek_leaves_position_unchanged() {
        let mut reader = make_reader(4096, 0);
        reader.seek(SeekFrom::Start(500)).unwrap();
        assert_eq!(reader.position, 500);
        let _ = reader.seek(SeekFrom::Current(i64::MIN));
        assert_eq!(
            reader.position, 500,
            "position must not change when a seek is rejected"
        );
    }
}
