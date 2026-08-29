use super::SECTOR_SIZE;
use anyhow::Error;
use serde::Serialize;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use tsify::{Ts, Tsify};
use wasm_bindgen::prelude::*;

/// XDVDFS root-offset candidates, in probe order. `iso_root_offset_candidates`
/// below names these directly, so order and values must stay in sync.
/// See `<https://free60.org/System-Software/Systems/GDFX>`
#[derive(Debug, Clone, Copy)]
pub enum IsoType {
    Xgd3,
    Xgd2,
    Xgd1,
    Xsf,
}

impl IsoType {
    pub fn name(self) -> &'static str {
        match self {
            Self::Xgd3 => "Xgd3",
            Self::Xgd2 => "Xgd2",
            Self::Xgd1 => "Xgd1",
            Self::Xsf => "Xsf",
        }
    }

    pub fn root_offset(self) -> u64 {
        match self {
            Self::Xgd3 => 0x0208_0000,
            Self::Xgd2 => 0x0fd9_0000,
            Self::Xgd1 => 0x1830_0000,
            Self::Xsf => 0,
        }
    }

    pub fn read<R: Read + Seek>(mut reader: R) -> Result<Option<Self>, Error> {
        if Self::check(&mut reader, Self::Xsf)? {
            return Ok(Some(Self::Xsf));
        }

        if Self::check(&mut reader, Self::Xgd2)? {
            return Ok(Some(Self::Xgd2));
        }

        if Self::check(&mut reader, Self::Xgd1)? {
            return Ok(Some(Self::Xgd1));
        }

        // Xgd3 is checked last, after every other known offset has failed.
        if Self::check(&mut reader, Self::Xgd3)? {
            return Ok(Some(Self::Xgd3));
        }

        Ok(None)
    }

    fn check<R: Read + Seek>(mut reader: R, iso_type: Self) -> Result<bool, Error> {
        let mut buf = [0_u8; 20];
        match reader
            .seek(SeekFrom::Start(0x20 * SECTOR_SIZE + iso_type.root_offset()))
            .and_then(|_| reader.read_exact(&mut buf))
        {
            Ok(()) => Ok(&buf == b"MICROSOFT*XBOX*MEDIA"),
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

/// One `(IsoType::name, IsoType::root_offset)` pair, as returned by
/// `iso_root_offset_candidates`.
#[derive(Debug, Clone, Serialize, Tsify)]
#[serde(rename_all = "camelCase")]
pub struct IsoRootOffsetCandidate {
    pub name: String,
    pub root_offset: u64,
}

/// Newtype so `Vec<IsoRootOffsetCandidate>` has a concrete,
/// `Tsify`-derivable name (`Ts<T>` needs `T: Tsify` directly).
/// `#[serde(transparent)]` keeps the JS shape a plain array.
#[derive(Debug, Clone, Serialize, Tsify)]
#[serde(transparent)]
pub struct IsoRootOffsetCandidates(pub Vec<IsoRootOffsetCandidate>);

/// The fixed set of root offsets probed when detecting where the XDVDFS
/// volume starts within a file, in probe order (Xsf, Xgd2, Xgd1, Xgd3) -
/// only these are ever tried, no sector-by-sector scan.
#[wasm_bindgen(js_name = isoRootOffsetCandidates)]
pub fn iso_root_offset_candidates() -> Result<Ts<IsoRootOffsetCandidates>, JsError> {
    let candidates: Vec<IsoRootOffsetCandidate> =
        [IsoType::Xsf, IsoType::Xgd2, IsoType::Xgd1, IsoType::Xgd3]
            .into_iter()
            .map(|iso_type| IsoRootOffsetCandidate {
                name: iso_type.name().to_owned(),
                root_offset: iso_type.root_offset(),
            })
            .collect();
    Ok(IsoRootOffsetCandidates(candidates).into_ts()?)
}
