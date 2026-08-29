use crate::core::writers::SliceWriter;
use std::io::{Read, Seek};
use xdvdfs::write::fs::{
    DefaultCopier, FileEntry, FilesystemCopier, FilesystemHierarchy, PathRef, XDVDFSFilesystem,
};

pub(crate) struct SortedFsForSlbd<R: Read + Seek + Send + Sync>(
    pub XDVDFSFilesystem<R, SliceWriter, DefaultCopier<R, SliceWriter>>,
);

impl<R> FilesystemHierarchy for SortedFsForSlbd<R>
where
    R: Read + Seek + Send + Sync,
    XDVDFSFilesystem<R, SliceWriter, DefaultCopier<R, SliceWriter>>: FilesystemHierarchy,
{
    type Error = <XDVDFSFilesystem<
        R,
        SliceWriter,
        DefaultCopier<R, SliceWriter>,
    > as FilesystemHierarchy>::Error;

    fn read_dir(&mut self, path: PathRef<'_>) -> Result<Vec<FileEntry>, Self::Error> {
        self.0.read_dir(path)
    }
}

impl<R> FilesystemCopier<SliceWriter> for SortedFsForSlbd<R>
where
    R: Read + Seek + Send + Sync,
    XDVDFSFilesystem<R, SliceWriter, DefaultCopier<R, SliceWriter>>: FilesystemCopier<SliceWriter>,
{
    type Error =
        <XDVDFSFilesystem<R, SliceWriter, DefaultCopier<R, SliceWriter>> as FilesystemCopier<
            SliceWriter,
        >>::Error;

    fn copy_file_in(
        &mut self,
        src: PathRef<'_>,
        dest: &mut SliceWriter,
        input_offset: u64,
        output_offset: u64,
        size: u64,
    ) -> Result<u64, Self::Error> {
        self.0
            .copy_file_in(src, dest, input_offset, output_offset, size)
    }
}

impl<R> FilesystemCopier<[u8]> for SortedFsForSlbd<R>
where
    R: Read + Seek + Send + Sync,
    XDVDFSFilesystem<R, SliceWriter, DefaultCopier<R, SliceWriter>>: FilesystemCopier<SliceWriter>,
{
    type Error =
        <XDVDFSFilesystem<R, SliceWriter, DefaultCopier<R, SliceWriter>> as FilesystemCopier<
            SliceWriter,
        >>::Error;

    fn copy_file_in(
        &mut self,
        src: PathRef<'_>,
        dest: &mut [u8],
        input_offset: u64,
        output_offset: u64,
        size: u64,
    ) -> Result<u64, Self::Error> {
        let mut tmp = SliceWriter::new(dest);
        let n = self
            .0
            .copy_file_in(src, &mut tmp, input_offset, output_offset, size)?;
        dest.copy_from_slice(&tmp.0[..dest.len()]);
        Ok(n)
    }
}
