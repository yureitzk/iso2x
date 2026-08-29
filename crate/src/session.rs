use crate::formats::cci::CciSession;
use crate::formats::ciso::CisoSession;
use crate::formats::extracted::ExtractedSession;
use crate::formats::god::GodSession;
use crate::formats::stfs::StfsWriteSession;
use crate::formats::xiso::XisoSession;
use crate::formats::zar::ZarSession;
use crate::utils::{JsErrExt, u64_to_js_number};
use enum_dispatch::enum_dispatch;
use js_sys::Uint8Array;
use serde::Serialize;
use tsify::{Ts, Tsify};
use wasm_bindgen::prelude::*;

/// Shared pull-based interface every format-specific session implements.
/// `hash_next_part` isn't part of this trait: three variants short-circuit
/// it to `Ok(true)` and `Stfs` calls a differently-named method
/// (`hash_next_block`), so that match stays hand-written in
/// `SessionInner`'s own `impl` block below.
#[enum_dispatch]
pub(crate) trait ChunkSource {
    /// Returns the next chunk of output bytes, or `None` once exhausted.
    /// `max_bytes` is a hint/cap, not a guarantee - some formats can only
    /// emit fixed-size units and may return more or less than requested.
    fn next_chunk(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, anyhow::Error>;
    fn is_done(&self) -> bool;
    /// Progress-reporting unit count; meaning varies by format (sectors,
    /// parts, files, etc).
    fn total_units(&self) -> u64;
    /// Authoritative count of `total_units()` completed so far, for
    /// formats where `next_chunk()`'s returned byte length isn't a valid
    /// proxy (e.g. zar: chunks are *compressed* output bytes, while
    /// `total_units()` is *raw input* bytes).
    ///
    /// `None` means the caller should keep deriving progress from
    /// received bytes instead, which is correct for every format but zar.
    fn units_done(&self) -> Option<u64> {
        None
    }
    /// Name of the output file the chunk just returned by `next_chunk`
    /// belongs to. `None` for formats that only ever produce one output
    /// stream.
    fn current_entry_name(&self) -> Option<&str> {
        None
    }
    /// Per-output-file (name, size) pairs, known up front at `open()` time.
    /// Empty if the format doesn't know its manifest yet (or ever).
    fn output_manifest(&self) -> Vec<(String, u64)> {
        Vec::new()
    }
}

/// Boxed variants need their own `ChunkSource` impl since `enum_dispatch`
/// calls through the field directly - one blanket impl covers all three.
impl<T: ChunkSource + ?Sized> ChunkSource for Box<T> {
    fn next_chunk(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, anyhow::Error> {
        (**self).next_chunk(max_bytes)
    }
    fn is_done(&self) -> bool {
        (**self).is_done()
    }
    fn total_units(&self) -> u64 {
        (**self).total_units()
    }
    fn units_done(&self) -> Option<u64> {
        (**self).units_done()
    }
    fn current_entry_name(&self) -> Option<&str> {
        (**self).current_entry_name()
    }
    fn output_manifest(&self) -> Vec<(String, u64)> {
        (**self).output_manifest()
    }
}

#[enum_dispatch(ChunkSource)]
pub(crate) enum SessionInner {
    God(Box<GodSession>),
    Xiso(XisoSession),
    Extracted(ExtractedSession),
    Ciso(CisoSession),
    Cci(CciSession),
    Zar(Box<ZarSession>),
    Stfs(Box<StfsWriteSession>),
}

impl SessionInner {
    /// Drives one incremental step of pre-streaming hashing/sizing, for
    /// formats that need one. Returns `true` once fully complete (or
    /// immediately, for formats with nothing to precompute).
    fn hash_next_part(&mut self) -> Result<bool, anyhow::Error> {
        match self {
            Self::God(s) => s.hash_next_part(),
            Self::Ciso(s) => s.hash_next_part(),
            Self::Cci(s) => s.hash_next_part(),
            Self::Stfs(s) => s.hash_next_block(),
            Self::Xiso(_) | Self::Extracted(_) | Self::Zar(_) => Ok(true),
        }
    }
}

/// One `{ name, size }` entry in a `ConversionSession::outputManifest()`
/// result.
#[derive(Debug, Clone, Serialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub struct OutputManifestEntry {
    pub name: String,
    pub size: u64,
}

/// `outputManifest()`'s actual return value: wraps `Vec<OutputManifestEntry>`
/// under a concrete name, since `Ts<T>` needs `T: Tsify` directly and there's
/// no blanket impl for `Vec<T>`. `#[serde(transparent)]` keeps the JS shape a
/// plain `OutputManifestEntry[]`.
#[derive(Debug, Clone, Serialize, Tsify)]
#[serde(transparent)]
pub struct OutputManifest(pub Vec<OutputManifestEntry>);

/// One format-agnostic session type exposed to JS. The worker drives this
/// the same way regardless of format - `nextChunk`/`isDone`/`totalUnits`
/// is the entire contract, with `hashNextPart` as an extra pre-streaming
/// step that's a no-op where nothing needs precomputing.
#[wasm_bindgen]
pub struct ConversionSession {
    inner: SessionInner,
}

#[wasm_bindgen]
impl ConversionSession {
    /// Drives one bounded step of pre-streaming hashing/sizing (see
    /// `SessionInner::hash_next_part`). Call this in a loop from JS,
    /// yielding to the event loop between calls, until it returns `true`,
    /// *before* calling `nextChunk()`.
    #[wasm_bindgen(js_name = hashNextPart)]
    pub fn hash_next_part(&mut self) -> Result<bool, JsError> {
        self.inner.hash_next_part().js_err()
    }

    #[wasm_bindgen(js_name = nextChunk)]
    pub fn next_chunk(&mut self, max_bytes: u32) -> Result<Option<Uint8Array>, JsError> {
        let chunk = self.inner.next_chunk(max_bytes as usize).js_err()?;
        Ok(chunk.map(|v| Uint8Array::from(v.as_slice())))
    }

    #[wasm_bindgen(js_name = isDone)]
    pub fn is_done(&self) -> bool {
        self.inner.is_done()
    }

    /// Sector, block, part, or file count, or (zar only) raw input byte
    /// total - see `ChunkSource::total_units`.
    #[wasm_bindgen(js_name = totalUnits)]
    pub fn total_units(&self) -> Result<f64, JsError> {
        u64_to_js_number(self.inner.total_units(), "total units").js_err()
    }

    #[wasm_bindgen(js_name = unitsDone)]
    pub fn units_done(&self) -> Result<Option<f64>, JsError> {
        self.inner
            .units_done()
            .map(|n| u64_to_js_number(n, "units done"))
            .transpose()
            .js_err()
    }

    #[wasm_bindgen(js_name = currentEntryName)]
    pub fn current_entry_name(&self) -> Option<String> {
        self.inner
            .current_entry_name()
            .map(std::borrow::ToOwned::to_owned)
    }

    /// Per-output-file entries for formats that produce multiple output
    /// files. Safe to call right after `open()`, before streaming - though
    /// it may be empty until sizing/hashing finishes, or permanently empty
    /// for single-stream formats.
    #[wasm_bindgen(js_name = outputManifest)]
    pub fn output_manifest(&self) -> Result<Ts<OutputManifest>, JsError> {
        let entries: Vec<OutputManifestEntry> = self
            .inner
            .output_manifest()
            .into_iter()
            .map(|(name, size)| OutputManifestEntry { name, size })
            .collect();
        Ok(OutputManifest(entries).into_ts()?)
    }
}

impl ConversionSession {
    pub(crate) fn new(inner: SessionInner) -> Self {
        Self { inner }
    }
}
