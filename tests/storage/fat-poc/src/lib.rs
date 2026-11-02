#![no_std]

pub mod retained;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bounds,
    Injected,
    Eof,
    WriteZero,
}
impl fatfs::IoError for Error {
    fn is_interrupted(&self) -> bool {
        false
    }
    fn new_unexpected_eof_error() -> Self {
        Self::Eof
    }
    fn new_write_zero_error() -> Self {
        Self::WriteZero
    }
}

/// PoC sector boundary. Errors commit no sectors for one call.
pub trait Sectors {
    fn capacity(&self) -> u64;
    fn read(&mut self, lba: u64, bytes: &mut [u8]) -> Result<(), Error>;
    fn write(&mut self, lba: u64, bytes: &[u8]) -> Result<(), Error>;
    fn sync(&mut self) -> Result<(), Error>;
}

impl<T: Sectors + ?Sized> Sectors for &mut T {
    fn capacity(&self) -> u64 {
        (**self).capacity()
    }
    fn read(&mut self, lba: u64, bytes: &mut [u8]) -> Result<(), Error> {
        (**self).read(lba, bytes)
    }
    fn write(&mut self, lba: u64, bytes: &[u8]) -> Result<(), Error> {
        (**self).write(lba, bytes)
    }
    fn sync(&mut self) -> Result<(), Error> {
        (**self).sync()
    }
}

pub struct Resident<T>(pub T);
impl<T: Sectors> resident_fat::BlockDevice for Resident<T> {
    type Error = Error;
    fn read(&mut self, lba: u64, b: &mut [u8]) -> Result<(), Error> {
        self.0.read(lba, b)
    }
    fn write(&mut self, lba: u64, b: &[u8]) -> Result<(), Error> {
        self.0.write(lba, b)
    }
    fn block_count(&mut self) -> Result<Option<u64>, Error> {
        Ok(Some(self.0.capacity()))
    }
    fn max_transfer_blocks(&self) -> u64 {
        8
    }
}

/// A byte cursor over sectors. A call stops at a sector boundary, so errors
/// never follow committed bytes in that call. Partial sectors use stack RMW.
pub struct Cursor<T> {
    pub device: T,
    position: u64,
}
impl<T: Sectors> Cursor<T> {
    pub fn new(device: T) -> Self {
        Self {
            device,
            position: 0,
        }
    }
    fn span(&self, wanted: usize) -> Result<(u64, usize, usize), Error> {
        let cap = self
            .device
            .capacity()
            .checked_mul(512)
            .ok_or(Error::Bounds)?;
        if self.position > cap {
            return Err(Error::Bounds);
        }
        let offset = (self.position % 512) as usize;
        let n = wanted.min(512 - offset).min((cap - self.position) as usize);
        Ok((self.position / 512, offset, n))
    }
}
impl<T: Sectors> fatfs::IoBase for Cursor<T> {
    type Error = Error;
}
impl<T: Sectors> fatfs::Read for Cursor<T> {
    fn read(&mut self, b: &mut [u8]) -> Result<usize, Error> {
        let (lba, off, n) = self.span(b.len())?;
        if n == 0 {
            return Ok(0);
        }
        if off == 0 && n == 512 {
            self.device.read(lba, &mut b[..n])?;
        } else {
            let mut sector = [0; 512];
            self.device.read(lba, &mut sector)?;
            b[..n].copy_from_slice(&sector[off..off + n]);
        }
        self.position += n as u64;
        Ok(n)
    }
}
impl<T: Sectors> fatfs::Write for Cursor<T> {
    fn write(&mut self, b: &[u8]) -> Result<usize, Error> {
        let (lba, off, n) = self.span(b.len())?;
        if n == 0 {
            return Ok(0);
        }
        if off == 0 && n == 512 {
            self.device.write(lba, &b[..n])?;
        } else {
            let mut sector = [0; 512];
            self.device.read(lba, &mut sector)?;
            sector[off..off + n].copy_from_slice(&b[..n]);
            self.device.write(lba, &sector)?;
        }
        self.position += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> Result<(), Error> {
        self.device.sync()
    }
}
impl<T: Sectors> fatfs::Seek for Cursor<T> {
    fn seek(&mut self, from: fatfs::SeekFrom) -> Result<u64, Error> {
        let cap = self
            .device
            .capacity()
            .checked_mul(512)
            .ok_or(Error::Bounds)?;
        let p = match from {
            fatfs::SeekFrom::Start(p) => Some(p),
            fatfs::SeekFrom::Current(d) => self.position.checked_add_signed(d),
            fatfs::SeekFrom::End(d) => cap.checked_add_signed(d),
        }
        .filter(|p| *p <= cap)
        .ok_or(Error::Bounds)?;
        self.position = p;
        Ok(p)
    }
}
