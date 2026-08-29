mod core;
mod formats;
mod game_list;
mod session;
mod utils;
use core::iso;
use core::scrub::ScrubMode;
use core::signing::ConsoleSigningKey;
use core::source::{self, FileType, SourceInfo, SourceInner, SourceOptions};
use core::title::ContentType;
pub use formats::chain_mht_digest;
use formats::{
    cci::CciSession,
    ciso::CisoSession,
    extracted::{ExtractedSession, XbePatchOptions},
    god::GodSession,
    stfs::{IdentityOverrides, StfsWriteSession},
    xiso::{XisoMode, XisoSession},
    zar::ZarSession,
};
use js_sys::{Function, Uint8Array};
use serde::Deserialize;
use session::{ConversionSession, SessionInner};
use tsify::{Ts, Tsify};
use utils::{JsErrExt, js_err, js_number_to_u64};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
fn init() {
    utils::set_panic_hook();
}

/// Thin `pub` shims into otherwise-private parsers, for `crate/fuzz`'s
/// cargo-fuzz targets. Only compiled with `--cfg fuzzing`.
#[cfg(fuzzing)]
pub mod fuzz_targets {
    use binrw::{BinRead, BinWrite};
    use std::io::Cursor;

    // Re-exported for structure-aware round-trip fuzzing via `arbitrary`.
    pub use crate::formats::cci::CciHeader;
    pub use crate::formats::stfs::StfsMetadata;
    pub use crate::formats::zar::ZarFooter;

    /// Every `CciHeader` field is a fixed-width int, so any value round-trips.
    pub fn cci_header_round_trips(header: CciHeader) {
        let mut buf = Vec::new();
        header
            .write(&mut Cursor::new(&mut buf))
            .expect("writing a fixed-size CciHeader into a Vec<u8> cannot fail");
        let parsed = CciHeader::read(&mut Cursor::new(&buf))
            .expect("bytes this target just wrote must parse back");
        assert_eq!(header, parsed, "CciHeader did not round-trip");
    }

    /// Same as `cci_header_round_trips`, for `.zar`'s footer.
    pub fn zar_footer_round_trips(footer: ZarFooter) {
        let mut buf = Vec::new();
        footer
            .write(&mut Cursor::new(&mut buf))
            .expect("writing a fixed-size ZarFooter into a Vec<u8> cannot fail");
        let parsed = ZarFooter::read(&mut Cursor::new(&buf))
            .expect("bytes this target just wrote must parse back");
        assert_eq!(footer, parsed, "ZarFooter did not round-trip");
    }

    /// `file_table_block_num` only ever writes 3 bytes on disk, so mask
    /// it before comparing - everything else round-trips exactly.
    pub fn stfs_metadata_round_trips(mut metadata: StfsMetadata) {
        metadata.volume_descriptor.file_table_block_num &= 0x00FF_FFFF;

        let mut buf = Vec::new();
        metadata
            .write(&mut Cursor::new(&mut buf))
            .expect("writing a fixed-size StfsMetadata into a Vec<u8> cannot fail");
        let parsed = StfsMetadata::read(&mut Cursor::new(&buf))
            .expect("bytes this target just wrote must parse back");
        assert_eq!(metadata, parsed, "StfsMetadata did not round-trip");
    }

    pub fn xbe_header(data: &[u8]) {
        let _ = crate::core::executable::xbe::XbeHeader::read(Cursor::new(data));
    }

    pub fn xex_header(data: &[u8]) {
        let _ = crate::core::executable::xex::XexHeader::read(Cursor::new(data));
    }

    pub fn dxt1_decode(width: u16, height: u16, data: &[u8]) {
        let _ = crate::core::texture::dxt1::decode_dxt1(u32::from(width), u32::from(height), data);
    }

    /// Full XPR0 container parse, including its dxt1/dxt3/swizzle decode.
    pub fn xpr_decode(data: &[u8]) {
        let _ = crate::core::texture::xpr::decode_xpr_to_png(data);
    }

    pub fn xdbf_resource(data: &[u8], id: u64, section: u16) {
        let _ = crate::core::xdbf::find_xdbf_resource(data, id, section);
    }

    /// Detects the XDVDFS root offset.
    pub fn xiso_probe_root_offset(data: &[u8]) {
        let _ = crate::core::iso::probe_root_offset_over(std::io::Cursor::new(data));
    }

    /// The XDVDFS directory-tree walk shared by every format's `inspect_source`.
    pub fn xiso_tree(data: &[u8]) {
        let _ = crate::core::iso::probe_source_over(std::io::Cursor::new(data));
    }

    pub fn cci_header(data: &[u8]) {
        crate::formats::cci::fuzz_parse_header_and_index(data);
    }

    pub fn ciso_header(data: &[u8]) {
        crate::formats::ciso::fuzz_parse_header_and_index(data);
    }

    /// GOD's fixed-size (4096-byte) master/subhash block.
    pub fn hash_list(data: &[u8]) {
        let _ = crate::formats::god::HashList::read(std::io::Cursor::new(data));
    }

    /// GOD's optional `CON`/`LIVE`/`PIRS` header, shared with STFS proper.
    pub fn stfs_header(data: &[u8]) {
        let mut reader = std::io::Cursor::new(data);
        if crate::formats::stfs::read_header_prefix(&mut reader).is_ok() {
            let _ = crate::formats::stfs::read_header_thumbnails(&mut reader);
        }
    }

    /// `.zar`'s file-tree walk: chunks `tree_bytes` into raw 16-byte
    /// entries and reconstructs paths against `name_table`.
    pub fn zar_tree(name_table: &[u8], tree_bytes: &[u8]) {
        let entries: Vec<_> = tree_bytes
            .chunks_exact(16)
            .map(|c| crate::formats::zar::parse_tree_entry(c.try_into().unwrap()))
            .collect();
        let _ = crate::formats::zar::build_files(name_table, &entries);
    }

    /// Chunks `data` into raw 0x40-byte file-listing entries and resolves them into paths.
    pub fn stfs_paths(data: &[u8]) {
        crate::formats::stfs::StfsReader::fuzz_build_paths(data);
    }

    /// `kind` selects `decode_dxt3` or one of the four `*_swizzled` decoders.
    pub fn texture_decode(kind: u8, width: u32, height: u32, data: &[u8]) {
        use crate::core::texture::{dxt3, swizzle};
        match kind % 6 {
            0 => {
                let _ = dxt3::decode_dxt3(width, height, data);
            }
            1 => {
                let _ = swizzle::decode_argb_swizzled(width, height, data);
            }
            2 => {
                let _ = swizzle::decode_rgb_swizzled(width, height, data);
            }
            3 => {
                let _ = swizzle::decode_rgba_swizzled(width, height, data);
            }
            4 => {
                let _ = swizzle::decode_r5g6b5_swizzled(width, height, data);
            }
            _ => {
                let _ = swizzle::decode_a4r4g4b4_swizzled(width, height, data);
            }
        }
    }
}

fn default_sectors_per_chunk() -> u32 {
    64
}

/// Suggested `outputName` for Xiso/Ciso/Cci/Zar targets, matching what a
/// GOD conversion of the same disc would title itself.
#[must_use]
#[wasm_bindgen(js_name = suggestDiscTitle)]
pub fn suggest_disc_title(base: &str, disc_number: u8, disc_count: u8) -> String {
    source::disc_suffixed_title(base, disc_number, disc_count)
}

/// XBE certificate `title_name` is fixed-width UTF-16LE, not UTF-8:
/// <https://xboxdevwiki.net/Xbe#Certificate>
fn utf16le_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Raw Xbox 360 console keyvault dump bytes for `God`/`Stfs` signing.
#[derive(Default, Deserialize, Tsify)]
#[tsify(type = "Uint8Array")]
pub struct SigningKeyBytes(#[serde(default, with = "serde_bytes")] Option<Vec<u8>>);

impl SigningKeyBytes {
    fn into_inner(self) -> Option<Vec<u8>> {
        self.0
    }
}

/// Raw 20-byte override for the `GoD`/STFS header's Device ID field
/// (metadata offset `0x3FD`): <https://free60.org/System-Software/Formats/STFS/#metadata>
#[derive(Default, Deserialize, Tsify)]
#[tsify(type = "Uint8Array")]
pub struct DeviceIdBytes(#[serde(default, with = "serde_bytes")] Option<Vec<u8>>);

impl DeviceIdBytes {
    fn into_device_id(self) -> Result<Option<[u8; 20]>, JsError> {
        self.0
            .map(|bytes| {
                <[u8; 20]>::try_from(bytes.as_slice()).map_err(|_| {
                    js_err(format!(
                        "deviceId must be exactly 20 bytes, got {}",
                        bytes.len()
                    ))
                })
            })
            .transpose()
    }
}

/// Raw 5-byte override for the STFS header's Console ID field (metadata
/// offset `0x36C`): <https://free60.org/System-Software/Formats/STFS/#metadata>
#[derive(Default, Deserialize, Tsify)]
#[tsify(type = "Uint8Array")]
pub struct ConsoleIdBytes(#[serde(default, with = "serde_bytes")] Option<Vec<u8>>);

impl ConsoleIdBytes {
    fn into_console_id(self) -> Result<Option<[u8; 5]>, JsError> {
        self.0
            .map(|bytes| {
                <[u8; 5]>::try_from(bytes.as_slice()).map_err(|_| {
                    js_err(format!(
                        "consoleId must be exactly 5 bytes, got {}",
                        bytes.len()
                    ))
                })
            })
            .transpose()
    }
}

/// Raw 8-byte override for the STFS header's Profile ID / XUID field
/// (metadata offset `0x371`). Display metadata only.
#[derive(Default, Deserialize, Tsify)]
#[tsify(type = "Uint8Array")]
pub struct ProfileIdBytes(#[serde(default, with = "serde_bytes")] Option<Vec<u8>>);

impl ProfileIdBytes {
    fn into_profile_id(self) -> Result<Option<[u8; 8]>, JsError> {
        self.0
            .map(|bytes| {
                <[u8; 8]>::try_from(bytes.as_slice()).map_err(|_| {
                    js_err(format!(
                        "profileId must be exactly 8 bytes, got {}",
                        bytes.len()
                    ))
                })
            })
            .transpose()
    }
}

/// Raw 8-byte override for the STFS header's Online Creator XUID field
/// (metadata offset `0x3AD`), distinct from Profile ID. Display metadata only.
#[derive(Default, Deserialize, Tsify)]
#[tsify(type = "Uint8Array")]
pub struct OnlineCreatorBytes(#[serde(default, with = "serde_bytes")] Option<Vec<u8>>);

impl OnlineCreatorBytes {
    fn into_online_creator(self) -> Result<Option<[u8; 8]>, JsError> {
        self.0
            .map(|bytes| {
                <[u8; 8]>::try_from(bytes.as_slice()).map_err(|_| {
                    js_err(format!(
                        "onlineCreator must be exactly 8 bytes, got {}",
                        bytes.len()
                    ))
                })
            })
            .transpose()
    }
}

/// Profile/save transfer options for `FormatOptions::Stfs::profile_transfer`.
#[derive(Default, Deserialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub struct ProfileTransferOptions {
    /// New gamertag to stamp into the source's `Account` file. Left
    /// untouched if omitted.
    #[serde(default)]
    new_gamertag: Option<String>,
    /// New XUID (8 bytes, big-endian - same shape as `profileId`) to
    /// stamp into the `Account` file. At least one of
    /// `newGamertag`/`newXuid` must be set.
    #[serde(default, rename = "newXuid")]
    new_xuid_bytes: ProfileIdBytes,
}

/// Write-side (target-format) options.
#[derive(Deserialize, Tsify)]
#[serde(tag = "format", rename_all = "camelCase")]
#[tsify(namespace)]
pub enum FormatOptions {
    #[serde(rename_all = "camelCase")]
    God {
        /// Defaults to `ScrubMode::default()`.
        #[serde(default)]
        mode: ScrubMode,
        /// Falls back to a games-list lookup by title ID when omitted.
        #[serde(default)]
        game_title: Option<String>,
        /// Produces a console-signed (`'CON '`) package instead of the
        /// default unsigned (`'LIVE'`) one. `GamesOnDemand` sources only.
        #[serde(default, rename = "signingKey")]
        signing_key_bytes: SigningKeyBytes,
        /// Overwrites the output header's Device ID field verbatim.
        /// Must be exactly 20 bytes if supplied.
        #[serde(default, rename = "deviceId")]
        device_id_bytes: DeviceIdBytes,
    },
    #[serde(rename_all = "camelCase")]
    Xiso {
        /// Defaults to `XisoMode::Full`.
        #[serde(default)]
        mode: XisoMode,
        /// Sectors read per streaming call, capped by the caller's own
        /// `maxBytes` argument to `nextChunk()`. Defaults to 64.
        #[serde(default = "default_sectors_per_chunk")]
        sectors_per_chunk: u32,
        /// Splits output past the ~4.28 GB FATX/FAT32 single-file limit.
        /// Requires `outputName`.
        #[serde(default)]
        split: bool,
        /// Filename stem for split parts, e.g. `"game"` -> `"game.1.xiso.iso"`, ...
        #[serde(default)]
        output_name: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Ciso {
        #[serde(default)]
        mode: ScrubMode,
        /// Filename stem for output files. Splits automatically past
        /// ~4 GiB (Stellar's CSO layout).
        output_name: String,
    },
    #[serde(rename_all = "camelCase")]
    Cci {
        #[serde(default)]
        mode: ScrubMode,
        /// Filename stem for output files. Splits automatically past
        /// ~4.28 GB, each `.N.cci` part self-contained.
        output_name: String,
    },
    #[serde(rename_all = "camelCase")]
    Extracted {
        /// Drops any file under `$SYSTEMUPDATE` from the output, before
        /// `default.xbe` is located.
        #[serde(default)]
        skip_system_update: bool,
        /// Patches the root `default.xbe` certificate's
        /// `allowed_media_types` to add HDD/media-board boot support.
        /// No-op if there's no root-level `default.xbe`.
        #[serde(default)]
        allowed_media_patch: bool,
        /// Overwrites the XBE certificate's `title_name` (UTF-16LE).
        /// Typically pre-filled from `inspectSource(...).detectedTitle`.
        #[serde(default)]
        rename_title: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Zar {
        /// Filename stem for the single output file. Never split.
        output_name: String,
    },
    #[serde(rename_all = "camelCase")]
    Stfs {
        /// Resolved in order: this override; the source's own STFS
        /// header; a launch-executable probe; `gamesOnDemand`.
        /// <https://free60.org/System-Software/Formats/STFS/#content-types>
        #[serde(default)]
        content_type: Option<ContentType>,
        /// Falls back to a games-list lookup by title ID when omitted.
        #[serde(default)]
        display_name: Option<String>,
        /// Overrides launch-executable detection. Bootable content
        /// types with no resolvable executable are a hard error;
        /// non-bootable ones fall back to `0`.
        #[serde(default)]
        title_id: Option<u32>,
        /// Produces a console-signed (`'CON '`) package, same as `God`'s `signingKey`.
        #[serde(default, rename = "signingKey")]
        signing_key_bytes: SigningKeyBytes,
        /// Overwrites the output header's Console ID field (5 bytes).
        /// Resolved: override, then source header, then zeroed.
        #[serde(default, rename = "consoleId")]
        console_id_bytes: ConsoleIdBytes,
        /// Overwrites the output header's Profile ID / XUID field (8
        /// bytes). Same resolution as `consoleId`. Display metadata only.
        #[serde(default, rename = "profileId")]
        profile_id_bytes: ProfileIdBytes,
        /// Overwrites the output header's Device ID field (20 bytes).
        /// Same resolution as `consoleId`.
        #[serde(default, rename = "deviceId")]
        device_id_bytes: DeviceIdBytes,
        /// Overwrites the output header's Online Creator XUID field (8
        /// bytes), distinct from `profileId`. Display metadata only.
        #[serde(default, rename = "onlineCreator")]
        online_creator_bytes: OnlineCreatorBytes,
        /// Rewrites the source package's `Account` file in place before
        /// conversion. Only valid for an already-extracted STFS profile
        /// package - rejected for an image-backed source. When set, the
        /// mutated account's XUID overrides `profileId`.
        #[serde(default, rename = "profileTransfer")]
        profile_transfer: Option<ProfileTransferOptions>,
    },
}

/// `None` is an error, not a silent default - the caller must resolve
/// `source` via `detectFormat`/`detectDirFormat` first.
fn resolve_source_options(source: Option<SourceOptions>) -> Result<SourceOptions, JsError> {
    source.ok_or_else(|| {
        js_err(
            "source format must be resolved first - call detectFormat() or \
             detectDirFormat() and pass the result as `source`",
        )
    })
}

fn open_source(
    read_fn: &source::SourceReadFnExtern,
    file_size: u64,
    source: Option<SourceOptions>,
    source_parts: &source::SourcePartsExtern,
    sequential_window: Option<usize>,
) -> Result<SourceInner, JsError> {
    let source_opts = resolve_source_options(source)?;
    let read_fn: &Function = read_fn.unchecked_ref();
    let parts = source::parts_from_js(source_parts, read_fn, file_size).js_err()?;
    source::open(&source_opts, parts, sequential_window).js_err()
}

fn xiso_session_from_source(
    opened: SourceInner,
    mode: XisoMode,
    sectors_per_chunk: u32,
    split: bool,
    output_name: Option<String>,
) -> Result<SessionInner, JsError> {
    let session = match opened {
        SourceInner::ExtractedFs(fs) => XisoSession::open_from_extracted(
            *fs,
            mode,
            sectors_per_chunk.max(1),
            split,
            output_name,
        ),
        image @ SourceInner::Image { .. } => image
            .into_image_source_with_probe()
            .map_err(|e| anyhow::anyhow!("xiso target: {e:#}"))
            .and_then(|(image_source, probed)| {
                XisoSession::open(
                    image_source,
                    mode,
                    sectors_per_chunk.max(1),
                    split,
                    output_name,
                    probed,
                )
            }),
    }
    .js_err()?;
    Ok(SessionInner::Xiso(session))
}

fn god_session_from_source(
    opened: SourceInner,
    mode: ScrubMode,
    game_title: Option<String>,
    signing_key_bytes: SigningKeyBytes,
    device_id_bytes: DeviceIdBytes,
) -> Result<SessionInner, JsError> {
    let signing_key = signing_key_bytes
        .into_inner()
        .map(|bytes| ConsoleSigningKey::parse(&bytes))
        .transpose()
        .map_err(|e| js_err(format!("invalid signingKey: {e:#}")))?;
    let device_id = device_id_bytes.into_device_id()?;
    let session = match opened {
        SourceInner::ExtractedFs(fs) => {
            GodSession::open_from_extracted(*fs, mode, game_title, signing_key, device_id)
        }
        image @ SourceInner::Image { .. } => image
            .into_image_source_with_probe()
            .map_err(|e| anyhow::anyhow!("god target: {e:#}"))
            .and_then(|(image_source, probed)| {
                GodSession::open(
                    image_source,
                    mode,
                    game_title,
                    signing_key,
                    device_id,
                    probed,
                )
            }),
    }
    .js_err()?;
    Ok(SessionInner::God(Box::new(session)))
}

/// Bundled wasm-facing STFS identity overrides, destructured from
/// `FormatOptions::Stfs`.
struct IdentityBytesOverrides {
    console_id: ConsoleIdBytes,
    profile_id: ProfileIdBytes,
    device_id: DeviceIdBytes,
    online_creator: OnlineCreatorBytes,
}

fn stfs_session_from_source(
    opened: SourceInner,
    content_type: Option<ContentType>,
    display_name: Option<String>,
    title_id: Option<u32>,
    signing_key_bytes: SigningKeyBytes,
    identity_bytes: IdentityBytesOverrides,
    profile_transfer: Option<ProfileTransferOptions>,
) -> Result<SessionInner, JsError> {
    let IdentityBytesOverrides {
        console_id: console_id_bytes,
        profile_id: profile_id_bytes,
        device_id: device_id_bytes,
        online_creator: online_creator_bytes,
    } = identity_bytes;
    let signing_key = signing_key_bytes
        .into_inner()
        .map(|bytes| ConsoleSigningKey::parse(&bytes))
        .transpose()
        .map_err(|e| js_err(format!("invalid signingKey: {e:#}")))?;
    let console_id = console_id_bytes.into_console_id()?;
    let mut profile_id = profile_id_bytes.into_profile_id()?;
    let device_id = device_id_bytes.into_device_id()?;
    let online_creator = online_creator_bytes.into_online_creator()?;

    // Profile transfer mutates the source's Account file and drives
    // profile_id below, so it happens before `opened` is consumed.
    let opened = match profile_transfer {
        None => opened,
        Some(transfer) => {
            let new_gamertag = transfer.new_gamertag;
            let new_xuid = transfer.new_xuid_bytes.into_profile_id()?;
            if new_gamertag.is_none() && new_xuid.is_none() {
                return Err(js_err(
                    "profileTransfer requires at least one of newGamertag/newXuid",
                ));
            }
            match opened {
                SourceInner::ExtractedFs(fs_box) => {
                    let mut fs = *fs_box;
                    let transferred_profile_id =
                        core::xprofile::transfer_account(&mut fs, |account| {
                            if let Some(gamertag) = new_gamertag {
                                account.gamertag = gamertag;
                            }
                            if let Some(xuid_bytes) = new_xuid {
                                account.xuid_online = u64::from_be_bytes(xuid_bytes);
                            }
                        })
                        .map_err(|e| js_err(format!("profileTransfer: {e:#}")))?;
                    profile_id = Some(transferred_profile_id);
                    SourceInner::ExtractedFs(Box::new(fs))
                }
                SourceInner::Image { .. } => {
                    return Err(js_err(
                        "profileTransfer requires an already-extracted STFS profile-package \
                         source (one with a root-level Account file) - not an image-backed one",
                    ));
                }
            }
        }
    };

    let identity_overrides = IdentityOverrides {
        console_id,
        profile_id,
        device_id,
        online_creator,
    };

    let session = match opened {
        SourceInner::ExtractedFs(fs) => StfsWriteSession::open_from_extracted(
            *fs,
            content_type,
            display_name,
            title_id,
            identity_overrides,
            signing_key,
        ),
        image @ SourceInner::Image { .. } => image
            .into_image_source_with_probe()
            .map_err(|e| anyhow::anyhow!("stfs target: {e:#}"))
            .and_then(|(image_source, probed)| {
                StfsWriteSession::open(
                    image_source,
                    content_type,
                    display_name,
                    title_id,
                    identity_overrides,
                    signing_key,
                    probed,
                )
            }),
    }
    .js_err()?;
    Ok(SessionInner::Stfs(Box::new(session)))
}

fn ciso_session_from_source(
    opened: SourceInner,
    output_name: String,
    mode: ScrubMode,
) -> Result<SessionInner, JsError> {
    let session = match opened {
        SourceInner::ExtractedFs(fs) => CisoSession::open_from_extracted(*fs, output_name, mode),
        image @ SourceInner::Image { .. } => image
            .into_image_source_with_probe()
            .map_err(|e| anyhow::anyhow!("ciso target: {e:#}"))
            .and_then(|(image_source, probed)| {
                CisoSession::open(image_source, output_name, mode, probed)
            }),
    }
    .js_err()?;
    Ok(SessionInner::Ciso(session))
}

fn cci_session_from_source(
    opened: SourceInner,
    output_name: String,
    mode: ScrubMode,
) -> Result<SessionInner, JsError> {
    let session = match opened {
        SourceInner::ExtractedFs(fs) => CciSession::open_from_extracted(*fs, output_name, mode),
        image @ SourceInner::Image { .. } => image
            .into_image_source_with_probe()
            .map_err(|e| anyhow::anyhow!("cci target: {e:#}"))
            .and_then(|(image_source, probed)| {
                CciSession::open(image_source, output_name, mode, probed)
            }),
    }
    .js_err()?;
    Ok(SessionInner::Cci(session))
}

fn extracted_session_from_source(
    opened: SourceInner,
    skip_system_update: bool,
    allowed_media_patch: bool,
    rename_title: Option<&str>,
) -> Result<SessionInner, JsError> {
    let xbe_patch = if allowed_media_patch || rename_title.is_some() {
        Some(XbePatchOptions {
            allowed_media_patch,
            rename_title: rename_title.map(utf16le_bytes),
        })
    } else {
        None
    };
    let session = match opened {
        SourceInner::ExtractedFs(fs) => Ok(ExtractedSession::open_from_extracted(
            *fs,
            skip_system_update,
            xbe_patch,
        )),
        image @ SourceInner::Image { .. } => image
            .into_image_source_with_probe()
            .map_err(|e| anyhow::anyhow!("extracted target: {e:#}"))
            .and_then(|(image_source, probed)| {
                ExtractedSession::open(image_source, skip_system_update, xbe_patch, probed)
            }),
    }
    .js_err()?;
    Ok(SessionInner::Extracted(session))
}

fn zar_session_from_source(
    opened: SourceInner,
    output_name: String,
) -> Result<SessionInner, JsError> {
    let session = match opened {
        SourceInner::ExtractedFs(fs) => ZarSession::open_from_extracted(*fs, output_name),
        image @ SourceInner::Image { .. } => image
            .into_image_source_with_probe()
            .map_err(|e| anyhow::anyhow!("zar target: {e:#}"))
            .and_then(|(image_source, probed)| ZarSession::open(image_source, output_name, probed)),
    }
    .js_err()?;
    Ok(SessionInner::Zar(Box::new(session)))
}

fn session_inner_from_opened(
    opened: SourceInner,
    options: FormatOptions,
) -> Result<SessionInner, JsError> {
    match options {
        FormatOptions::Xiso {
            mode,
            sectors_per_chunk,
            split,
            output_name,
        } => xiso_session_from_source(opened, mode, sectors_per_chunk, split, output_name),
        FormatOptions::God {
            mode,
            game_title,
            signing_key_bytes,
            device_id_bytes,
        } => god_session_from_source(opened, mode, game_title, signing_key_bytes, device_id_bytes),
        FormatOptions::Ciso { mode, output_name } => {
            ciso_session_from_source(opened, output_name, mode)
        }
        FormatOptions::Cci { mode, output_name } => {
            cci_session_from_source(opened, output_name, mode)
        }
        FormatOptions::Extracted {
            skip_system_update,
            allowed_media_patch,
            rename_title,
        } => extracted_session_from_source(
            opened,
            skip_system_update,
            allowed_media_patch,
            rename_title.as_deref(),
        ),
        FormatOptions::Zar { output_name } => zar_session_from_source(opened, output_name),
        FormatOptions::Stfs {
            content_type,
            display_name,
            title_id,
            signing_key_bytes,
            console_id_bytes,
            profile_id_bytes,
            device_id_bytes,
            online_creator_bytes,
            profile_transfer,
        } => stfs_session_from_source(
            opened,
            content_type,
            display_name,
            title_id,
            signing_key_bytes,
            IdentityBytesOverrides {
                console_id: console_id_bytes,
                profile_id: profile_id_bytes,
                device_id: device_id_bytes,
                online_creator: online_creator_bytes,
            },
            profile_transfer,
        ),
    }
}

/// Opens a conversion session for `source` targeting `options.format`.
///
/// `sequential_window` is the readahead window (bytes) for the bulk
/// sequential pass. Only meaningful for `god`/`xiso`/`ciso`/`cci`
/// sources. Falls back to `core::reader::DEFAULT_SEQ_WINDOW` (8 MiB).
///
/// # Errors
///
/// Returns an error if `options` fails to deserialize, if `signingKey` is
/// invalid, if `source` is unresolved, or if the resolved source can't be
/// opened as the requested target format.
#[wasm_bindgen(js_name = openConversionSession)]
pub fn open_conversion_session(
    read_fn: &source::SourceReadFnExtern,
    file_size: f64,
    options: &Ts<FormatOptions>,
    source: Option<Ts<SourceOptions>>,
    source_parts: &source::SourcePartsExtern,
    sequential_window: Option<usize>,
) -> Result<ConversionSession, JsError> {
    let options: FormatOptions = options.to_rust()?;
    let source: Option<SourceOptions> = source.map(|s| s.to_rust()).transpose()?;
    let file_size = js_number_to_u64(file_size, "fileSize").js_err()?;
    let opened = open_source(read_fn, file_size, source, source_parts, sequential_window)?;
    let inner = session_inner_from_opened(opened, options)?;
    Ok(ConversionSession::new(inner))
}

/// A source that's already been opened - container located, and (for a
/// raw XISO) its XDVDFS root already found. Opaque to JS: hold the
/// handle, call methods on it, `free()` when done.
#[wasm_bindgen]
pub struct OpenedSource(SourceInner);

impl OpenedSource {
    pub(crate) fn from_inner(inner: SourceInner) -> Self {
        Self(inner)
    }
}

/// Opens `source` once. Pass the result to `OpenedSource::inspect()`,
/// `OpenedSource::generateAttachXbe()`, and/or
/// `OpenedSource::openConversionSession()`, which all reuse this
/// already-open, already-probed source.
///
/// # Errors
/// Same failure modes as `inspectSource`.
#[wasm_bindgen(js_name = openSource)]
pub fn open_source_js(
    read_fn: &source::SourceReadFnExtern,
    file_size: f64,
    source: Option<Ts<SourceOptions>>,
    source_parts: &source::SourcePartsExtern,
    sequential_window: Option<usize>,
) -> Result<OpenedSource, JsError> {
    let source: Option<SourceOptions> = source.map(|s| s.to_rust()).transpose()?;
    let file_size = js_number_to_u64(file_size, "fileSize").js_err()?;
    let opened = open_source(read_fn, file_size, source, source_parts, sequential_window)?;
    Ok(OpenedSource(opened))
}

#[wasm_bindgen]
impl OpenedSource {
    /// Same as the standalone `inspectSource`, against an already-open
    /// source. The directory-tree walk is cached on `self` for reuse by
    /// a following `openConversionSession` call.
    ///
    /// # Errors
    /// Same as `inspectSource`.
    #[wasm_bindgen(js_name = inspect)]
    pub fn inspect_js(&mut self, include_thumbnail: bool) -> Result<Ts<SourceInfo>, JsError> {
        let info = match &mut self.0 {
            SourceInner::ExtractedFs(fs) => {
                core::source::inspect_extracted(fs, include_thumbnail).js_err()
            }
            SourceInner::Image { .. } => {
                core::source::inspect_source(&mut self.0, include_thumbnail).js_err()
            }
        }?;
        Ok(info.into_ts()?)
    }

    /// Same as the standalone `generateAttachXbe`, against an already-open source.
    ///
    /// # Errors
    /// Same as `generateAttachXbe`.
    #[wasm_bindgen(js_name = generateAttachXbe)]
    pub fn generate_attach_xbe_js(&mut self) -> Result<Uint8Array, JsError> {
        generate_attach_xbe_from(&mut self.0)
    }

    /// Opens a conversion session directly from this already-open source,
    /// consuming the handle. Reuses any directory-tree walk cached by a
    /// prior `inspect()` call.
    ///
    /// # Errors
    /// Same as `openConversionSession`.
    #[wasm_bindgen(js_name = openConversionSession)]
    pub fn open_conversion_session_js(
        self,
        options: &Ts<FormatOptions>,
    ) -> Result<ConversionSession, JsError> {
        let options: FormatOptions = options.to_rust()?;
        let inner = session_inner_from_opened(self.0, options)?;
        Ok(ConversionSession::new(inner))
    }
}

/// Shows title/content-type for a source, before its conversion session
/// is opened. Image-backed sources are inspected via the XDVDFS root;
/// an extracted source via `default.xbe`/`default.xex`.
///
/// # Errors
///
/// Returns an error if `source` is unresolved, if reading `source_parts`
/// fails, or if the launch executable / XDVDFS root can't be located.
#[wasm_bindgen(js_name = inspectSource)]
pub fn inspect_source(
    read_fn: &source::SourceReadFnExtern,
    file_size: f64,
    source: Option<Ts<SourceOptions>>,
    source_parts: &source::SourcePartsExtern,
    include_thumbnail: bool,
) -> Result<Ts<SourceInfo>, JsError> {
    let source: Option<SourceOptions> = source.map(|s| s.to_rust()).transpose()?;
    let file_size = js_number_to_u64(file_size, "fileSize").js_err()?;
    // Metadata-only: never enters Sequential mode, so no window param.
    let mut opened = open_source(read_fn, file_size, source, source_parts, None)?;
    let info = match &mut opened {
        SourceInner::ExtractedFs(fs) => {
            core::source::inspect_extracted(fs, include_thumbnail).js_err()
        }
        SourceInner::Image { .. } => {
            core::source::inspect_source(&mut opened, include_thumbnail).js_err()
        }
    }?;
    Ok(info.into_ts()?)
}

/// Single-file, magic-byte detection path (Xiso/Ciso/Cci). For a dropped
/// folder use `detectDirFormat` instead.
///
/// # Errors
///
/// Returns an error if reading from `read_fn` fails or if the file's magic
/// bytes don't match any known single-file format.
#[wasm_bindgen(js_name = detectFormat)]
pub fn js_detect_format(
    read_fn: source::SourceReadFnExtern,
    file_size: f64,
) -> Result<Ts<FileType>, JsError> {
    let file_size = js_number_to_u64(file_size, "fileSize").js_err()?;
    let read_fn: Function = read_fn.unchecked_into();
    let format = source::detect(read_fn, file_size).js_err()?;
    Ok(format.into_ts()?)
}

/// Wraps `Option<FileType>` so it derives `Tsify` directly; `transparent`
/// keeps the JS shape `FileType | undefined`, not an object.
#[derive(Debug, Clone, Copy, serde::Serialize, Tsify)]
#[serde(transparent)]
pub struct DetectedDirFormat(pub Option<FileType>);

/// Directory-shape detection path (God/Extracted). Takes the flat list of
/// forward-slash-normalized relative paths for every regular file in the
/// dropped folder. Returns `None` when the listing matches neither shape.
///
/// # Errors
///
/// Returns an error if the detected result fails to serialize across the wasm boundary.
// `entries` can't be taken by reference: wasm-bindgen has no FromWasmAbi impl for &[String].
#[allow(clippy::needless_pass_by_value)]
#[wasm_bindgen(js_name = detectDirFormat)]
pub fn js_detect_dir_format(entries: Vec<String>) -> Result<Ts<DetectedDirFormat>, JsError> {
    Ok(DetectedDirFormat(source::detect_dir(&entries)).into_ts()?)
}

/// Builds a bootable "default.xbe" stub for an OGX source, so a converted
/// ISO/CCI/CISO can still launch from a softmod dashboard. Rejects `GoD` (XEX) sources.
fn generate_attach_xbe_from(opened: &mut SourceInner) -> Result<Uint8Array, JsError> {
    let source_xbe_bytes: Vec<u8> = match opened {
        SourceInner::ExtractedFs(fs) => {
            let (exe_bytes, is_xex) = fs.read_launch_executable().js_err()?;
            if is_xex {
                return Err(js_err(
                    "attach XBE can only be generated for OGX sources - this is a GoD (XEX) source",
                ));
            }
            exe_bytes
        }
        SourceInner::Image { .. } => {
            let probed = opened.probed().js_err()?;
            let title_info = &probed.title_info;
            if !matches!(title_info.content_type, ContentType::XboxOriginal) {
                return Err(js_err(
                    "attach XBE can only be generated for OGX sources - this is a GoD (XEX) source",
                ));
            }
            let xbe_entry = probed
                .directory_table
                .entries
                .iter()
                .find(|e| !e.is_directory() && e.path.eq_ignore_ascii_case("default.xbe"))
                .ok_or_else(|| js_err("no default.xbe found at the root of this image"))?;
            let (sector, size) = (xbe_entry.sector, xbe_entry.size);
            let SourceInner::Image { source, .. } = opened else {
                unreachable!("checked above")
            };
            source::validate_entry_size(source.as_ref(), sector, size).js_err()?;
            let mut buf = vec![0u8; size as usize];
            source
                .read_bytes(u64::from(sector) * iso::SECTOR_SIZE, &mut buf)
                .js_err()?;
            buf
        }
    };

    let attach_xbe = core::attach_xbe::build_attach_xbe(&source_xbe_bytes).js_err()?;
    Ok(Uint8Array::from(attach_xbe.as_slice()))
}

/// Standalone counterpart to `OpenedSource::generateAttachXbe`.
///
/// # Errors
///
/// Returns an error if `source` is unresolved, if the source is a `GoD`
/// (XEX) source rather than OGX, if the XDVDFS root or `default.xbe`
/// can't be located, or if building the attach stub fails.
#[wasm_bindgen(js_name = generateAttachXbe)]
pub fn generate_attach_xbe(
    read_fn: &source::SourceReadFnExtern,
    file_size: f64,
    source: Option<Ts<SourceOptions>>,
    source_parts: &source::SourcePartsExtern,
) -> Result<Uint8Array, JsError> {
    let source: Option<SourceOptions> = source.map(|s| s.to_rust()).transpose()?;
    let file_size = js_number_to_u64(file_size, "fileSize").js_err()?;
    let mut opened = open_source(read_fn, file_size, source, source_parts, None)?;
    generate_attach_xbe_from(&mut opened)
}
