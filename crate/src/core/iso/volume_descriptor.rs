use super::SECTOR_SIZE;
use super::iso_type::IsoType;
use anyhow::{Error, format_err};
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug)]
pub struct VolumeDescriptor {
    pub root_offset: u64,
    pub sector_size: u64,
    pub root_directory_sector: u32,
    pub root_directory_size: u32,
    pub volume_size: u64,
    pub volume_sectors: u64,
    pub iso_type: IsoType,
}

impl VolumeDescriptor {
    pub fn read<R: Read + Seek + Send + Sync>(mut reader: R) -> Result<VolumeDescriptor, Error> {
        let iso_type =
            IsoType::read(&mut reader)?.ok_or_else(|| format_err!("invalid ISO format"))?;
        Self::read_of_type(reader, iso_type)
    }

    fn read_of_type<R: Read + Seek + Send + Sync>(
        mut reader: R,
        iso_type: IsoType,
    ) -> Result<VolumeDescriptor, Error> {
        let root_offset = iso_type.root_offset();

        let mut dev = xdvdfs::blockdev::OffsetWrapper::new_with_provided_offset(
            &mut reader,
            xdvdfs_offset_of(iso_type),
        );
        let volume = xdvdfs::read::read_volume(&mut dev)
            .map_err(|e| format_err!("failed to parse XDVDFS volume descriptor: {e:?}"))?;

        let reader_len = {
            let cur = reader.stream_position()?;
            let end = reader.seek(SeekFrom::End(0))?;
            reader.seek(SeekFrom::Start(cur))?;
            end
        };

        let volume_size = reader_len - root_offset;
        let volume_sectors = volume_size / SECTOR_SIZE;

        Ok(VolumeDescriptor {
            sector_size: SECTOR_SIZE,
            root_offset,
            root_directory_sector: volume.root_table.region.sector,
            root_directory_size: volume.root_table.region.size,
            volume_size,
            volume_sectors,
            iso_type,
        })
    }
}

pub(super) fn xdvdfs_offset_of(iso_type: IsoType) -> xdvdfs::blockdev::XDVDFSOffsets {
    use xdvdfs::blockdev::XDVDFSOffsets;
    match iso_type {
        IsoType::Xsf => XDVDFSOffsets::XISO,
        IsoType::Xgd1 => XDVDFSOffsets::XGD1,
        IsoType::Xgd2 => XDVDFSOffsets::XGD2,
        IsoType::Xgd3 => XDVDFSOffsets::XGD3,
    }
}
