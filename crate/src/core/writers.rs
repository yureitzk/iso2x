use std::io;
use xdvdfs::blockdev::BlockDeviceWrite;

pub(crate) struct SliceWriter(pub(crate) Vec<u8>);

impl SliceWriter {
    pub fn new(dest: &mut [u8]) -> Self {
        Self(dest.to_vec())
    }
}

impl BlockDeviceWrite for SliceWriter {
    type WriteError = io::Error;

    fn write(&mut self, offset: u64, buffer: &[u8]) -> Result<(), io::Error> {
        let offset =
            usize::try_from(offset).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let end = offset + buffer.len();

        if self.0.len() < end {
            self.0.resize(end, 0);
        }

        self.0[offset..end].copy_from_slice(buffer);
        Ok(())
    }

    fn len(&mut self) -> Result<u64, io::Error> {
        Ok(self.0.len() as u64)
    }
}
