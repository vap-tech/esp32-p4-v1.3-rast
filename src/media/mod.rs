//! Small allocation-free media readers. No filesystem or transport policy.
#[cfg(feature = "software-jpeg")]
pub mod jpeg;
pub mod wav;

#[derive(Debug)]
pub enum Error<E> {
    Io(E),
    Invalid(&'static str),
    Unsupported(&'static str),
}

pub trait Input {
    type Error;
    fn len(&self) -> u32;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn position(&self) -> u32;
    fn seek(&mut self, position: u32) -> Result<(), Self::Error>;
    fn read(&mut self, out: &mut [u8]) -> Result<usize, Self::Error>;
}
impl<D: crate::storage::BlockDevice> Input for crate::storage::fat32::File<'_, D> {
    type Error = crate::storage::Error<D::Error>;
    fn len(&self) -> u32 {
        self.len()
    }
    fn position(&self) -> u32 {
        self.position()
    }
    fn seek(&mut self, position: u32) -> Result<(), Self::Error> {
        self.seek(position)
    }
    fn read(&mut self, out: &mut [u8]) -> Result<usize, Self::Error> {
        self.read(out)
    }
}
impl<D: crate::storage::BlockDevice> Input for crate::storage::fat32::SharedFile<'_, D> {
    type Error = crate::storage::Error<D::Error>;
    fn len(&self) -> u32 {
        self.len()
    }
    fn position(&self) -> u32 {
        self.position()
    }
    fn seek(&mut self, position: u32) -> Result<(), Self::Error> {
        self.seek(position)
    }
    fn read(&mut self, out: &mut [u8]) -> Result<usize, Self::Error> {
        self.read(out)
    }
}

pub fn read_exact<R: Input>(input: &mut R, mut out: &mut [u8]) -> Result<(), Error<R::Error>> {
    while !out.is_empty() {
        let n = input.read(out).map_err(Error::Io)?;
        if n == 0 {
            return Err(Error::Invalid("truncated file"));
        }
        out = &mut out[n..];
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    pub struct Bytes<'a> {
        pub data: &'a [u8],
        pub pos: u32,
    }
    impl super::Input for Bytes<'_> {
        type Error = ();
        fn len(&self) -> u32 {
            self.data.len() as u32
        }
        fn position(&self) -> u32 {
            self.pos
        }
        fn seek(&mut self, pos: u32) -> Result<(), ()> {
            if pos > self.len() {
                return Err(());
            }
            self.pos = pos;
            Ok(())
        }
        fn read(&mut self, out: &mut [u8]) -> Result<usize, ()> {
            let n = out.len().min(self.data.len() - self.pos as usize);
            out[..n].copy_from_slice(&self.data[self.pos as usize..self.pos as usize + n]);
            self.pos += n as u32;
            Ok(n)
        }
    }
}

#[cfg(feature = "software-jpeg")]
pub mod idct;
#[cfg(feature = "software-jpeg")]
mod loeffler;
