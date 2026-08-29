use crate::core::executable::{TitleExecutionInfo, Xex360Version, xbe, xex};
use crate::core::iso::IsoReader;
use crate::game_list;
use anyhow::{Context, Error, bail};
use num_enum::TryFromPrimitive;
use std::io::{Read, Seek};
use tsify::{Ts, Tsify};
use wasm_bindgen::prelude::*;

/// STFS package content type, from the metadata header.
/// `<https://free60.org/System-Software/Formats/STFS/#content-types>`
#[repr(u32)]
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Tsify, TryFromPrimitive,
)]
#[serde(rename_all = "camelCase")]
pub enum ContentType {
    /// Xbox 360 disc/GoD content, boots a `default.xex`.
    GamesOnDemand = 0x7000,
    /// Original Xbox (OGX) content, boots a `default.xbe`.
    XboxOriginal = 0x5000,
    /// Xbox Live Arcade content, also boots a `default.xex`. Indistinguishable
    /// from `GamesOnDemand` by executable alone - producing it from a
    /// non-STFS source needs an explicit `contentType` override
    /// (`FormatOptions::Stfs::content_type` in `lib.rs`).
    ArcadeGame = 0xD0000,
    Xbox360Title = 0x1000,
    InstalledGame = 0x4000,
    GameDemo = 0x80000,
    Video = 0x90000,
    CommunityGame = 0x0200_0000,
    #[serde(rename = "xna")]
    Xna = 0xE0000,
    SavedGame = 1,
    MarketPlaceContent = 2,
    Publisher = 3,
    IptvPauseBuffer = 0x2000,
    AvatarAssetPack = 0x8000,
    AvatarItem = 0x9000,
    Profile = 0x10000,
    GamerPicture = 0x20000,
    Theme = 0x30000,
    CacheFile = 0x40000,
    StorageDownload = 0x50000,
    XboxSavedGame = 0x60000,
    XboxDownload = 0x70000,
    GamerTitle = 0xA0000,
    Installer = 0xB0000,
    GameTrailer = 0xC0000,
    LicenseStore = 0xF0000,
    Movie = 0x0010_0000,
    Tv = 0x0020_0000,
    MusicVideo = 0x0030_0000,
    GameVideo = 0x0040_0000,
    PodcastVideo = 0x0050_0000,
    ViralVideo = 0x0060_0000,
}

impl ContentType {
    pub(crate) fn from_u32(value: u32) -> Option<Self> {
        Self::try_from(value).ok()
    }

    /// Whether this content type is expected to carry a launch executable
    /// (`default.xex`/`default.xbe`). Types that return `false` fall back
    /// to `title_id = 0` when no executable is found.
    pub(crate) fn requires_launch_executable(self) -> bool {
        matches!(
            self,
            Self::GamesOnDemand
                | Self::XboxOriginal
                | Self::ArcadeGame
                | Self::Xbox360Title
                | Self::InstalledGame
                | Self::GameDemo
                | Self::CommunityGame
                | Self::Xna
        )
    }

    /// Which family of `ContentType` this belongs to - lets a caller
    /// cheaply answer "should I bother setting `titleId` for this content
    /// type?" without independently knowing STFS domain trivia. Kept next
    /// to `requires_launch_executable` so the two classifications can't
    /// silently drift apart.
    pub(crate) fn family(self) -> ContentFamily {
        if self.requires_launch_executable() {
            ContentFamily::Bootable
        } else {
            match self {
                Self::Profile => ContentFamily::ProfileAccount,
                Self::SavedGame
                | Self::XboxSavedGame
                | Self::MarketPlaceContent
                | Self::AvatarItem
                | Self::Installer => ContentFamily::TitleAttached,
                _ => ContentFamily::StandaloneAsset,
            }
        }
    }
}

/// See `ContentType::family`. `Bootable` types boot a launch executable
/// directly; `ProfileAccount` is GPD-bearing profile data (currently
/// just `Profile`); `TitleAttached` types name a *parent* title via
/// `titleId` without being bootable themselves; `StandaloneAsset`
/// covers everything else (themes, gamer pictures, movies, and other
/// content with no meaningful parent title).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub enum ContentFamily {
    Bootable,
    ProfileAccount,
    TitleAttached,
    StandaloneAsset,
}

/// JS-facing accessor for `ContentType::family`.
#[wasm_bindgen(js_name = contentTypeFamily)]
pub fn content_type_family(content_type: Ts<ContentType>) -> Result<Ts<ContentFamily>, JsError> {
    let content_type: ContentType = content_type.to_rust()?;
    Ok(content_type.family().into_ts()?)
}

/// Structured title version. Xex and Xbe versions have genuinely
/// different shapes - a packed major/minor/build/qfe for Xex, a flat
/// build counter with no such structure for Xbe (see
/// `TitleExecutionInfo::xex_version`'s doc comment) - so this stays a
/// tagged union rather than one struct with fields that don't apply to
/// half its variants.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, Tsify)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TitleVersion {
    Xex {
        version: Xex360Version,
        /// `None` when `base_version` is all-zero, i.e. this isn't a patch.
        base: Option<Xex360Version>,
    },
    Xbe {
        build: u32,
    },
}

impl std::fmt::Display for TitleVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Xex {
                version,
                base: Some(base),
            } => write!(f, "{version} (base {base})"),
            Self::Xex {
                version,
                base: None,
            } => write!(f, "{version}"),
            Self::Xbe { build } => write!(f, "{build}"),
        }
    }
}

/// JS-facing formatter for `TitleVersion`. Callers that want the
/// structured fields instead should read them directly off
/// `SourceInfo::version`.
// wasm_bindgen exports receive owned values from the JS side; there's no
// reference to take instead.
#[allow(clippy::needless_pass_by_value)]
#[wasm_bindgen(js_name = formatTitleVersion)]
pub fn format_title_version(version: Ts<TitleVersion>) -> Result<String, JsError> {
    let version: TitleVersion = version.to_rust()?;
    Ok(version.to_string())
}

/// Looks up a game title by its Xbox title ID.
#[must_use]
#[wasm_bindgen(js_name = lookupTitleById)]
pub fn lookup_title_by_id(title_id: u32) -> Option<String> {
    game_list::find_title_by_id(title_id)
}

// `Clone` lets a cached `ProbedDirectoryTable` (see `core::source`) hand
// an owned copy to each caller that reuses it, instead of every reuse
// needing its own borrow of the same probe.
#[derive(Clone)]
pub struct TitleInfo {
    pub content_type: ContentType,
    pub execution_info: TitleExecutionInfo,
}

impl TitleInfo {
    /// Locates the launch executable off an already-open XDVDFS image and
    /// parses its header.
    pub fn from_image<R: Read + Seek + Send + Sync>(
        iso_image: &mut IsoReader<R>,
    ) -> Result<TitleInfo, Error> {
        if let Some(mut executable) = iso_image.entry(&"\\default.xex".into())? {
            let default_xex_header =
                xex::XexHeader::read(&mut executable).context("error reading default.xex")?;
            let execution_info = default_xex_header
                .fields
                .execution_info
                .context("no execution info in default.xex header")?;

            Ok(TitleInfo {
                content_type: ContentType::GamesOnDemand,
                execution_info,
            })
        } else if let Some(mut executable) = iso_image.entry(&"\\default.xbe".into())? {
            let default_xbe_header =
                xbe::XbeHeader::read(&mut executable).context("error reading default.xbe")?;
            let execution_info = default_xbe_header
                .fields
                .execution_info
                .context("no execution info in default.xbe header")?;

            Ok(TitleInfo {
                content_type: ContentType::XboxOriginal,
                execution_info,
            })
        } else {
            bail!("no executable found in this image");
        }
    }

    /// Structured version info. XEX titles carry the packed
    /// major.minor.build.qfe encoding (plus an optional base version for
    /// patches); XBE titles have no such structure, so they're reported
    /// as the flat build number they actually are. See `TitleVersion`.
    pub fn version(&self) -> TitleVersion {
        if self.content_type == ContentType::XboxOriginal {
            TitleVersion::Xbe {
                build: self.execution_info.version,
            }
        } else {
            let base = self.execution_info.xex_base_version();
            TitleVersion::Xex {
                version: self.execution_info.xex_version(),
                base: (!base.is_zero()).then_some(base),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn from_u32_recognizes_games_on_demand() {
        assert_eq!(
            ContentType::from_u32(0x7000),
            Some(ContentType::GamesOnDemand)
        );
    }
    #[test]
    fn from_u32_recognizes_xbox_original() {
        assert_eq!(
            ContentType::from_u32(0x5000),
            Some(ContentType::XboxOriginal)
        );
    }
    #[test]
    fn from_u32_recognizes_arcade_game() {
        assert_eq!(
            ContentType::from_u32(0xD0000),
            Some(ContentType::ArcadeGame)
        );
    }
    #[test]
    fn from_u32_returns_none_for_unrecognized_content_type() {
        assert_eq!(ContentType::from_u32(0x00FF_0000), None);
    }
    #[test]
    fn from_u32_returns_none_for_zero() {
        assert_eq!(ContentType::from_u32(0), None);
    }
    #[test]
    fn from_u32_recognizes_xbox_360_title() {
        assert_eq!(
            ContentType::from_u32(0x1000),
            Some(ContentType::Xbox360Title)
        );
    }
    #[test]
    fn from_u32_recognizes_installed_game() {
        assert_eq!(
            ContentType::from_u32(0x4000),
            Some(ContentType::InstalledGame)
        );
    }
    #[test]
    fn from_u32_recognizes_game_demo() {
        assert_eq!(ContentType::from_u32(0x80000), Some(ContentType::GameDemo));
    }
    #[test]
    fn from_u32_recognizes_community_game() {
        assert_eq!(
            ContentType::from_u32(0x2000000),
            Some(ContentType::CommunityGame)
        );
    }
    #[test]
    fn from_u32_recognizes_xna() {
        assert_eq!(ContentType::from_u32(0xE0000), Some(ContentType::Xna));
    }
    #[test]
    fn from_u32_recognizes_movie() {
        assert_eq!(ContentType::from_u32(0x100000), Some(ContentType::Movie));
    }
    #[test]
    fn from_u32_recognizes_gamer_picture() {
        assert_eq!(
            ContentType::from_u32(0x20000),
            Some(ContentType::GamerPicture)
        );
    }
    #[test]
    fn from_u32_recognizes_saved_game() {
        assert_eq!(ContentType::from_u32(1), Some(ContentType::SavedGame));
    }
    #[test]
    fn from_u32_recognizes_theme() {
        assert_eq!(ContentType::from_u32(0x30000), Some(ContentType::Theme));
    }
    #[test]
    fn from_u32_recognizes_tv() {
        assert_eq!(ContentType::from_u32(0x200000), Some(ContentType::Tv));
    }
    #[test]
    fn from_u32_recognizes_video() {
        assert_eq!(ContentType::from_u32(0x90000), Some(ContentType::Video));
    }
    #[test]
    fn requires_launch_executable_is_true_only_for_the_original_bootable_set() {
        for ct in [
            ContentType::GamesOnDemand,
            ContentType::XboxOriginal,
            ContentType::ArcadeGame,
            ContentType::Xbox360Title,
            ContentType::InstalledGame,
            ContentType::GameDemo,
            ContentType::CommunityGame,
            ContentType::Xna,
        ] {
            assert!(ct.requires_launch_executable(), "{ct:?} should be bootable");
        }
        for ct in [
            ContentType::SavedGame,
            ContentType::Movie,
            ContentType::GamerPicture,
            ContentType::Theme,
            ContentType::Profile,
        ] {
            assert!(
                !ct.requires_launch_executable(),
                "{ct:?} should not be bootable"
            );
        }
    }

    #[test]
    fn family_matches_expected_groupings() {
        for ct in [
            ContentType::GamesOnDemand,
            ContentType::XboxOriginal,
            ContentType::ArcadeGame,
            ContentType::Xbox360Title,
            ContentType::InstalledGame,
            ContentType::GameDemo,
            ContentType::CommunityGame,
            ContentType::Xna,
        ] {
            assert_eq!(ct.family(), ContentFamily::Bootable, "{ct:?}");
        }
        assert_eq!(ContentType::Profile.family(), ContentFamily::ProfileAccount);
        for ct in [
            ContentType::SavedGame,
            ContentType::XboxSavedGame,
            ContentType::MarketPlaceContent,
            ContentType::AvatarItem,
            ContentType::Installer,
        ] {
            assert_eq!(ct.family(), ContentFamily::TitleAttached, "{ct:?}");
        }
        for ct in [
            ContentType::Theme,
            ContentType::GamerPicture,
            ContentType::Movie,
            ContentType::Tv,
            ContentType::Video,
        ] {
            assert_eq!(ct.family(), ContentFamily::StandaloneAsset, "{ct:?}");
        }
    }

    #[test]
    fn version_xbe_is_flat_build_number() {
        let info = TitleInfo {
            content_type: ContentType::XboxOriginal,
            execution_info: TitleExecutionInfo {
                media_id: 0,
                version: 42,
                base_version: 0,
                title_id: 0,
                platform: 0,
                executable_type: 0,
                disc_number: 1,
                disc_count: 1,
                save_game_id: 0,
            },
        };
        assert!(matches!(info.version(), TitleVersion::Xbe { build: 42 }));
    }

    #[test]
    fn version_xex_omits_base_when_zero() {
        let info = TitleInfo {
            content_type: ContentType::GamesOnDemand,
            execution_info: TitleExecutionInfo {
                media_id: 0,
                version: 0x1234_5678,
                base_version: 0,
                title_id: 0,
                platform: 0,
                executable_type: 0,
                disc_number: 1,
                disc_count: 1,
                save_game_id: 0,
            },
        };
        assert!(matches!(
            info.version(),
            TitleVersion::Xex { base: None, .. }
        ));
    }
}
