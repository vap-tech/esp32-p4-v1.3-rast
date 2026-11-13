//! Filesystem-neutral, serialized async API. FAT is the first backend.
//! Direct cursors hold a volume lease; files::FileTable multiplexes owned contexts.
//! No dynamic VFS or heap.
// Filesystem access is serialized by the volume API; this borrow is the IO lease.
#![allow(clippy::await_holding_refcell_ref)]
use super::asynchronous::{self as io, BlockDevice, Shared, Stream};
use core::cell::{Cell, RefCell};
use embedded_io_async::{Read, Seek, SeekFrom, Write};
pub const NAME_BYTES: usize = 768;
pub const PATH_BYTES: usize = 768;
type Fat<'io, D> = embedded_fatfs::FileSystem<
    Shared<'io, D>,
    embedded_fatfs::DefaultTimeProvider,
    embedded_fatfs::LossyOemCpConverter,
>;
pub(super) type FatFile<'a, 'io, D> = embedded_fatfs::File<
    'a,
    Shared<'io, D>,
    embedded_fatfs::DefaultTimeProvider,
    embedded_fatfs::LossyOemCpConverter,
>;
type FatIter<'a, 'io, D> = embedded_fatfs::DirIter<
    'a,
    Shared<'io, D>,
    embedded_fatfs::DefaultTimeProvider,
    embedded_fatfs::LossyOemCpConverter,
>;
#[derive(Debug)]
pub enum Error<E: core::fmt::Debug> {
    Fs(embedded_fatfs::Error<io::Error<E>>),
    InvalidPath,
    AlreadyExists,
    NotFile,
    NeedsRecovery,
    InvalidHandle,
    TooManyOpenFiles,
    Busy,
    AccessDenied,
    GenerationExhausted,
}
impl<E: core::fmt::Debug> From<embedded_fatfs::Error<io::Error<E>>> for Error<E> {
    fn from(e: embedded_fatfs::Error<io::Error<E>>) -> Self {
        Self::Fs(e)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
}
pub struct Entry {
    pub name: [u8; NAME_BYTES],
    pub name_len: usize,
    pub kind: Kind,
    pub len: u64,
}
impl Entry {
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap()
    }
}
pub enum Volume<'io, D: BlockDevice> {
    Fat(FatVolume<'io, D>),
}
pub struct FatVolume<'io, D: BlockDevice> {
    pub(super) fs: Fat<'io, D>,
    pub(super) stream: &'io RefCell<Stream<D>>,
    pub(super) poisoned: Cell<bool>,
    pub(super) dirty_at_mount: bool,
}
pub(super) struct Mutation<'a>(&'a Cell<bool>);
impl<'a> Mutation<'a> {
    pub(super) fn begin(flag: &'a Cell<bool>) -> Self {
        flag.set(true);
        Self(flag)
    }
    pub(super) fn commit(self) {
        self.0.set(false);
    }
}
pub(super) fn path<E: core::fmt::Debug>(p: &str) -> Result<&str, Error<E>> {
    let p = p.trim_start_matches('/');
    if p.len() > PATH_BYTES
        || p.split('/').any(|c| {
            c == "."
                || c == ".."
                || c.is_empty()
                || c.ends_with([' ', '.'])
                || c.encode_utf16().count() > 255
        })
        || p.chars().any(|c| {
            c.is_control() || matches!(c, '\u{ffff}' | '\u{fffe}') || "\\:*?\"<>|".contains(c)
        })
    {
        return Err(Error::InvalidPath);
    }
    Ok(p)
}
impl<'io, D: BlockDevice> Volume<'io, D> {
    pub async fn mount(stream: &'io mut RefCell<Stream<D>>) -> Result<Self, Error<D::Error>> {
        stream
            .borrow_mut()
            .seek(SeekFrom::Start(0))
            .await
            .map_err(embedded_fatfs::Error::Io)?;
        let fs = Fat::new(
            Shared(stream),
            embedded_fatfs::FsOptions::new().update_accessed_date(false),
        )
        .await?;
        let flags = fs.read_status_flags().await?;
        Ok(Self::Fat(FatVolume {
            fs,
            stream,
            poisoned: Cell::new(false),
            dirty_at_mount: flags.dirty() || flags.io_error(),
        }))
    }
    pub(super) fn fat(&self) -> &FatVolume<'io, D> {
        match self {
            Self::Fat(f) => f,
        }
    }
    pub fn capacity(&self) -> u64 {
        self.fat().stream.borrow().capacity()
    }
    pub fn needs_recovery(&self) -> bool {
        self.fat().dirty_at_mount || self.fat().poisoned.get()
    }
    pub(super) fn readable(&self) -> Result<(), Error<D::Error>> {
        if self.fat().poisoned.get() {
            Err(Error::NeedsRecovery)
        } else {
            Ok(())
        }
    }
    pub(super) fn writable(&self) -> Result<(), Error<D::Error>> {
        if self.needs_recovery() {
            Err(Error::NeedsRecovery)
        } else {
            Ok(())
        }
    }
    async fn checkpoint(&self) -> Result<(), Error<D::Error>> {
        self.fat().fs.flush().await?;
        self.fat()
            .stream
            .borrow_mut()
            .flush()
            .await
            .map_err(embedded_fatfs::Error::Io)?;
        Ok(())
    }
    pub async fn sync(&mut self) -> Result<(), Error<D::Error>> {
        self.readable()?;
        if self.fat().dirty_at_mount {
            return Ok(());
        }
        self.checkpoint().await
    }
    pub async fn open(&mut self, p: &str) -> Result<File<'_, 'io, D>, Error<D::Error>> {
        self.readable()?;
        let p = path(p)?;
        let meta = self.fat().fs.root_dir().open_meta(p).await?;
        if !meta.is_file() {
            return Err(Error::NotFile);
        }
        let len = meta.len();
        let file = meta.to_file();
        Ok(File { file, len })
    }
    pub async fn read_dir(&mut self, p: &str) -> Result<Directory<'_, 'io, D>, Error<D::Error>> {
        self.readable()?;
        let fat = self.fat();
        let root = fat.fs.root_dir();
        let dir = if matches!(p, "" | "/") {
            root
        } else {
            root.open_dir(path(p)?).await?
        };
        Ok(Directory { iter: dir.iter() })
    }
    pub async fn create_new(&mut self, p: &str) -> Result<Writer<'_, 'io, D>, Error<D::Error>> {
        self.writable()?;
        let p = path(p)?;
        let fat = self.fat();
        match fat.fs.root_dir().open_meta(p).await {
            Ok(_) => return Err(Error::AlreadyExists),
            Err(embedded_fatfs::Error::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
        let guard = Mutation::begin(&fat.poisoned);
        let file = fat.fs.root_dir().create_file(p).await?;
        let mut name = [0; PATH_BYTES];
        name[..p.len()].copy_from_slice(p.as_bytes());
        Ok(Writer {
            file: Some(file),
            fat,
            path: name,
            path_len: p.len(),
            guard: Some(guard),
            len: 0,
        })
    }
    pub async fn create_dir(&mut self, p: &str) -> Result<(), Error<D::Error>> {
        self.writable()?;
        let p = path(p)?;
        let f = self.fat();
        match f.fs.root_dir().open_meta(p).await {
            Ok(_) => return Err(Error::AlreadyExists),
            Err(embedded_fatfs::Error::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
        let guard = Mutation::begin(&f.poisoned);
        f.fs.root_dir().create_dir(p).await?;
        self.checkpoint().await?;
        guard.commit();
        Ok(())
    }
    pub async fn remove(&mut self, p: &str) -> Result<(), Error<D::Error>> {
        self.writable()?;
        let p = path(p)?;
        let f = self.fat();
        // Resolve and reject nonempty directories before starting mutation.
        let entry = f.fs.root_dir().open_meta(p).await?;
        if entry.is_dir() && !entry.to_dir().is_empty().await? {
            return Err(embedded_fatfs::Error::DirectoryIsNotEmpty.into());
        }
        let guard = Mutation::begin(&f.poisoned);
        f.fs.root_dir().remove(p).await?;
        self.checkpoint().await?;
        guard.commit();
        Ok(())
    }
    pub async fn rename(&mut self, source: &str, dest: &str) -> Result<(), Error<D::Error>> {
        self.writable()?;
        let source = path(source)?;
        let dest = path(dest)?;
        let f = self.fat();
        f.fs.root_dir().open_meta(source).await?;
        match f.fs.root_dir().open_meta(dest).await {
            Ok(_) => return Err(Error::AlreadyExists),
            Err(embedded_fatfs::Error::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
        let guard = Mutation::begin(&f.poisoned);
        f.fs.root_dir()
            .rename(source, &f.fs.root_dir(), dest)
            .await?;
        self.checkpoint().await?;
        guard.commit();
        Ok(())
    }
}
pub struct File<'a, 'io, D: BlockDevice> {
    file: FatFile<'a, 'io, D>,
    len: u64,
}
impl<D: BlockDevice> File<'_, '_, D> {
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub async fn read(&mut self, b: &mut [u8]) -> Result<usize, Error<D::Error>> {
        Ok(self.file.read(b).await?)
    }
    pub async fn seek(&mut self, n: u64) -> Result<(), Error<D::Error>> {
        if n > self.len {
            return Err(Error::InvalidPath);
        }
        self.file.seek(SeekFrom::Start(n)).await?;
        Ok(())
    }
}
pub struct Directory<'a, 'io, D: BlockDevice> {
    iter: FatIter<'a, 'io, D>,
}
impl<D: BlockDevice> Directory<'_, '_, D> {
    pub async fn next(&mut self) -> Option<Result<Entry, Error<D::Error>>> {
        loop {
            let e = match self.iter.next().await? {
                Ok(e) => e,
                Err(e) => return Some(Err(e.into())),
            };
            let mut name = [0; NAME_BYTES];
            let mut len = 0;
            if let Some(units) = e.long_file_name_as_ucs2_units() {
                for c in core::char::decode_utf16(units.iter().copied()) {
                    let mut b = [0; 4];
                    let c = c.unwrap_or(core::char::REPLACEMENT_CHARACTER);
                    let text = c.encode_utf8(&mut b);
                    if len + text.len() > name.len() {
                        return Some(Err(Error::InvalidPath));
                    }
                    name[len..len + text.len()].copy_from_slice(text.as_bytes());
                    len += text.len();
                }
            } else {
                let bytes = e.short_file_name_as_bytes();
                len = bytes.len();
                name[..len].copy_from_slice(bytes);
                if !bytes.is_ascii() {
                    return Some(Err(Error::InvalidPath));
                }
            }
            if matches!(&name[..len], b"." | b"..") {
                continue;
            }
            return Some(Ok(Entry {
                name,
                name_len: len,
                kind: if e.is_dir() {
                    Kind::Directory
                } else {
                    Kind::File
                },
                len: e.len(),
            }));
        }
    }
}
/// Dropping/cancelling an unfinished writer poisons the volume. The disk's dirty
/// state is deliberately preserved; only finish/explicit abort can clear it.
pub struct Writer<'a, 'io, D: BlockDevice> {
    file: Option<FatFile<'a, 'io, D>>,
    fat: &'a FatVolume<'io, D>,
    path: [u8; PATH_BYTES],
    path_len: usize,
    guard: Option<Mutation<'a>>,
    len: u64,
}
impl<D: BlockDevice> Writer<'_, '_, D> {
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub async fn write(&mut self, b: &[u8]) -> Result<(), Error<D::Error>> {
        self.file.as_mut().unwrap().write_all(b).await?;
        self.len += b.len() as u64;
        Ok(())
    }
    pub async fn finish(mut self) -> Result<(), Error<D::Error>> {
        self.file.take().unwrap().close().await?;
        self.fat.fs.flush().await?;
        self.fat
            .stream
            .borrow_mut()
            .flush()
            .await
            .map_err(embedded_fatfs::Error::Io)?;
        self.guard.take().unwrap().commit();
        Ok(())
    }
    pub async fn abort(mut self) -> Result<(), Error<D::Error>> {
        self.file.take().unwrap().close().await?;
        let p = core::str::from_utf8(&self.path[..self.path_len]).unwrap();
        self.fat.fs.root_dir().remove(p).await?;
        self.fat.fs.flush().await?;
        self.fat
            .stream
            .borrow_mut()
            .flush()
            .await
            .map_err(embedded_fatfs::Error::Io)?;
        self.guard.take().unwrap().commit();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    use std::{collections::BTreeMap, rc::Rc};
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Fault;
    struct Disk {
        sectors: u64,
        blocks: BTreeMap<u64, [u8; 512]>,
        yields: Rc<Cell<usize>>,
        fail: Rc<Cell<bool>>,
    }
    impl Disk {
        fn new(sectors: u64) -> Self {
            Self {
                sectors,
                blocks: BTreeMap::new(),
                yields: Rc::new(Cell::new(0)),
                fail: Rc::new(Cell::new(false)),
            }
        }
    }
    impl crate::storage::BlockDevice for Disk {
        type Error = Fault;
        fn sectors(&self) -> u64 {
            self.sectors
        }
        fn max_transfer_bytes(&self) -> usize {
            8192
        }
        fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), Fault> {
            for (i, b) in out.chunks_mut(512).enumerate() {
                b.copy_from_slice(self.blocks.get(&(lba + i as u64)).unwrap_or(&[0; 512]));
            }
            Ok(())
        }
        fn write(&mut self, lba: u64, b: &[u8]) -> Result<(), Fault> {
            if self.fail.get() {
                return Err(Fault);
            }
            for (i, b) in b.chunks(512).enumerate() {
                self.blocks.insert(lba + i as u64, b.try_into().unwrap());
            }
            Ok(())
        }
    }
    async fn yield_once(counter: &Cell<usize>) {
        let mut pending = true;
        core::future::poll_fn(|cx| {
            if pending {
                pending = false;
                counter.set(counter.get() + 1);
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await
    }
    impl BlockDevice for Disk {
        type Error = Fault;
        fn sectors(&self) -> u64 {
            self.sectors
        }
        fn max_transfer_bytes(&self) -> usize {
            8192
        }
        async fn read(&mut self, lba: u64, b: &mut [u8]) -> Result<(), Fault> {
            yield_once(&self.yields).await;
            crate::storage::BlockDevice::read(self, lba, b)
        }
        async fn write(&mut self, lba: u64, b: &[u8]) -> Result<(), Fault> {
            yield_once(&self.yields).await;
            crate::storage::BlockDevice::write(self, lba, b)
        }
        async fn sync(&mut self) -> Result<(), Fault> {
            yield_once(&self.yields).await;
            Ok(())
        }
    }
    fn run<F: Future>(f: F) -> F::Output {
        let mut f = core::pin::pin!(f);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(r) = f.as_mut().poll(&mut cx) {
                return r;
            }
        }
    }
    fn disk() -> Disk {
        let mut d = Disk::new(131072);
        let mut scratch = [0; 8192];
        crate::storage::format::quick_format(&mut d, &mut scratch, 42, |_, _| {}).unwrap();
        d
    }
    #[test]
    fn long_names_streams_abort_rename_remount_and_independent_image() {
        let disk = disk();
        let yields = disk.yields.clone();
        let mut stream = RefCell::new(Stream::new(run(io::Partition::mbr(disk, 0)).unwrap()));
        run(async {
            let mut v = Volume::mount(&mut stream).await.unwrap();
            assert!(!v.needs_recovery());
            v.create_dir("/Музыка и записи").await.unwrap();
            let contents: std::vec::Vec<u8> = (0..32771).map(|i| i as u8).collect();
            let mut w = v
                .create_new("/Музыка и записи/Длинное имя 🎵.bin")
                .await
                .unwrap();
            w.write(&contents[..77]).await.unwrap();
            w.write(&contents[77..]).await.unwrap();
            w.finish().await.unwrap();
            let mut f = v.open("/Музыка и записи/Длинное имя 🎵.bin").await.unwrap();
            assert_eq!(f.len(), 32771);
            let mut out = std::vec![0;32771];
            let mut at = 0;
            while at < out.len() {
                at += f.read(&mut out[at..]).await.unwrap();
            }
            assert_eq!(out, contents);
            f.seek(777).await.unwrap();
            let mut tail = [0; 1003];
            let mut n = 0;
            while n < tail.len() {
                n += f.read(&mut tail[n..]).await.unwrap();
            }
            assert_eq!(&tail, &contents[777..1780]);
            drop(f);
            v.rename(
                "/Музыка и записи/Длинное имя 🎵.bin",
                "/Музыка и записи/переименовано.bin",
            )
            .await
            .unwrap();
            let mut dir = v.read_dir("/Музыка и записи").await.unwrap();
            let e = dir.next().await.unwrap().unwrap();
            assert_eq!(e.name(), "переименовано.bin");
            assert!(dir.next().await.is_none());
            drop(dir);
            let mut w = v.create_new("/Оборванная загрузка.bin").await.unwrap();
            w.write(b"unfinished").await.unwrap();
            w.abort().await.unwrap();
            assert!(v.open("/Оборванная загрузка.bin").await.is_err());
            v.sync().await.unwrap();
            drop(v);
            let mut v = Volume::mount(&mut stream).await.unwrap();
            assert!(!v.needs_recovery());
            assert_eq!(
                v.open("/Музыка и записи/переименовано.bin")
                    .await
                    .unwrap()
                    .len(),
                32771
            );
            // Valid FAT LFN can exceed 255 UTF-8 bytes while fitting UTF-16.
            let name = std::format!("/{}.txt", "Ж".repeat(200));
            let mut w = v.create_new(&name).await.unwrap();
            w.write(b"long utf8 name").await.unwrap();
            w.finish().await.unwrap();
            v.create_dir("/destination").await.unwrap();
            v.rename(&name, "/destination/moved.txt").await.unwrap();
            assert_eq!(v.open("/destination/moved.txt").await.unwrap().len(), 14);
            v.remove("/destination/moved.txt").await.unwrap();
            v.remove("/destination").await.unwrap();
            assert!(v.create_dir("/../invalid").await.is_err());
            assert!(!v.needs_recovery());
            v.sync().await.unwrap();
        });
        assert!(yields.get() > 100, "underlying IO must really yield");
        let partition = stream.into_inner().into_inner();
        let disk = partition.device;
        std::fs::create_dir_all("target/storage").unwrap();
        let mut file = std::fs::File::create("target/storage/async-fat32.img").unwrap();
        use std::io::{Seek, SeekFrom, Write};
        file.set_len(partition.sectors * 512).unwrap();
        for (lba, b) in disk.blocks {
            if lba >= partition.start && lba < partition.start + partition.sectors {
                file.seek(SeekFrom::Start((lba - partition.start) * 512))
                    .unwrap();
                file.write_all(&b).unwrap();
            }
        }
    }
    #[test]
    fn five_handles_interleave_images_logs_and_readers_without_stale_reuse() {
        use crate::storage::files::{FileTable, OpenMode};
        let mut stream = RefCell::new(Stream::new(run(io::Partition::mbr(disk(), 0)).unwrap()));
        run(async {
            let mut volume = Volume::mount(&mut stream).await.unwrap();
            let mut source = volume.create_new("/Исходная картинка.bin").await.unwrap();
            source.write(b"0123456789").await.unwrap();
            source.finish().await.unwrap();
            let alias = {
                let entry = volume
                    .fat()
                    .fs
                    .root_dir()
                    .open_meta("Исходная картинка.bin")
                    .await
                    .unwrap();
                std::string::String::from_utf8(entry.short_file_name_as_bytes().into()).unwrap()
            };
            let mut files = FileTable::<_, 5>::new(&mut volume);
            assert_eq!(files.capacity(), 5);
            assert!(files
                .open("/missing/child.bin", OpenMode::CreateNew)
                .await
                .is_err());
            assert!(!files.needs_recovery());
            let a = files
                .open("/image-a.bin", OpenMode::CreateNew)
                .await
                .unwrap();
            let b = files
                .open("/image-b.bin", OpenMode::CreateNew)
                .await
                .unwrap();
            let temp = files
                .open("/temperature.log", OpenMode::Append)
                .await
                .unwrap();
            let errors = files.open("/errors.log", OpenMode::Append).await.unwrap();
            let read = files
                .open("/Исходная картинка.bin", OpenMode::Read)
                .await
                .unwrap();
            assert_eq!(files.open_count(), 5);
            assert!(matches!(
                files.open("/extra.bin", OpenMode::CreateNew).await,
                Err(Error::TooManyOpenFiles)
            ));
            assert!(matches!(
                files.write(read, b"wrong").await,
                Err(Error::AccessDenied)
            ));
            assert!(matches!(
                files.seek(temp, 0).await,
                Err(Error::AccessDenied)
            ));
            let block = [0xa7; 16384];
            for _ in 0..12 {
                assert_eq!(files.write(a, &block).await.unwrap(), 8192);
                assert_eq!(files.write(b, &block[..1003]).await.unwrap(), 1003);
                files.write(temp, b"23.5\n").await.unwrap();
                files.write(errors, b"alert\n").await.unwrap();
                let mut byte = [0];
                files.read(read, &mut byte).await.unwrap();
            }
            files.seek(read, 3).await.unwrap();
            let mut part = [0; 4];
            assert_eq!(files.read(read, &mut part).await.unwrap(), 4);
            assert_eq!(&part, b"3456");
            files.close(b).await.unwrap();
            let second_reader = files.open(&alias, OpenMode::Read).await.unwrap();
            let mut first = [0];
            files.read(second_reader, &mut first).await.unwrap();
            assert_eq!(&first, b"0");
            assert_eq!(files.info(read).unwrap().position, 7);
            files.close(second_reader).await.unwrap();
            assert!(matches!(
                files.open(&alias, OpenMode::Append).await,
                Err(Error::Busy)
            ));
            assert!(matches!(
                files.open("/ERRORS.LOG", OpenMode::Read).await,
                Err(Error::Busy)
            ));
            files.sync(temp).await.unwrap();
            assert!(matches!(
                files.rename("/image-a.bin", "/moved.bin").await,
                Err(Error::Busy)
            ));
            assert_eq!(files.info(a).unwrap().len, 12 * 8192);
            files.close(a).await.unwrap();
            let replacement = files.open("/new.bin", OpenMode::CreateNew).await.unwrap();
            assert!(matches!(files.info(a), Err(Error::InvalidHandle)));
            files.abort(replacement).await.unwrap();
            files.close(temp).await.unwrap();
            files.close(errors).await.unwrap();
            files.close(read).await.unwrap();
            assert_eq!(files.open_count(), 0);
            let reopened = files
                .open("/temperature.log", OpenMode::Append)
                .await
                .unwrap();
            assert_eq!(files.info(reopened).unwrap().position, 60);
            files.write(reopened, b"24.0\n").await.unwrap();
            files.close(reopened).await.unwrap();
            drop(files);
            let mut one = FileTable::<_, 1>::new(&mut volume);
            let fresh = one.open("/temperature.log", OpenMode::Read).await.unwrap();
            assert!(matches!(
                one.read(read, &mut part).await,
                Err(Error::InvalidHandle)
            ));
            one.close(fresh).await.unwrap();
            drop(one);
            assert!(volume.open("/extra.bin").await.is_err());
            assert!(volume.open("/new.bin").await.is_err());
            let mut f = volume.open("/temperature.log").await.unwrap();
            let mut out = [0; 65];
            let mut n = 0;
            while n < out.len() {
                n += f.read(&mut out[n..]).await.unwrap();
            }
            assert_eq!(&out[60..], b"24.0\n");
            drop(f);
            assert_eq!(volume.open("/image-b.bin").await.unwrap().len(), 12 * 1003);
            drop(volume);
            assert!(!Volume::mount(&mut stream).await.unwrap().needs_recovery());
        });
        let partition = stream.into_inner().into_inner();
        std::fs::create_dir_all("target/storage").unwrap();
        let mut image = std::fs::File::create("target/storage/multifile-fat32.img").unwrap();
        use std::io::{Seek, SeekFrom, Write};
        image.set_len(partition.sectors * 512).unwrap();
        for (lba, bytes) in partition.device.blocks {
            if lba >= partition.start && lba < partition.start + partition.sectors {
                image
                    .seek(SeekFrom::Start((lba - partition.start) * 512))
                    .unwrap();
                image.write_all(&bytes).unwrap();
            }
        }
    }
    #[test]
    fn directory_alias_locks_and_unrelated_metadata_with_active_writer() {
        use crate::storage::files::{FileTable, OpenMode};
        let mut stream = RefCell::new(Stream::new(run(io::Partition::mbr(disk(), 0)).unwrap()));
        run(async {
            let mut volume = Volume::mount(&mut stream).await.unwrap();
            volume.create_dir("/Длинная папка").await.unwrap();
            volume.create_dir("/Длинная папка/child").await.unwrap();
            let alias = {
                let e = volume
                    .fat()
                    .fs
                    .root_dir()
                    .open_meta("Длинная папка")
                    .await
                    .unwrap();
                std::string::String::from_utf8(e.short_file_name_as_bytes().into()).unwrap()
            };
            let mut files = FileTable::<_, 3>::new(&mut volume);
            let log = files
                .open("/Длинная папка/log.txt", OpenMode::Append)
                .await
                .unwrap();
            files.write(log, b"log record").await.unwrap();
            let other = files.open("/other.bin", OpenMode::CreateNew).await.unwrap();
            files.write(other, b"unrelated").await.unwrap();
            files.close(other).await.unwrap();
            files.rename("/other.bin", "/renamed.bin").await.unwrap();
            files.remove("/renamed.bin").await.unwrap();
            assert!(matches!(files.remove(&alias).await, Err(Error::Busy)));
            assert!(matches!(
                files.rename(&alias, "/moved").await,
                Err(Error::Busy)
            ));
            files.close(log).await.unwrap();
            assert!(matches!(
                files.rename(&alias, "/Длинная папка/child/cycle").await,
                Err(Error::InvalidPath)
            ));
            assert!(!files.is_poisoned());
            drop(files);
            assert_eq!(
                volume.open("/Длинная папка/log.txt").await.unwrap().len(),
                10
            );
        });
    }
    #[test]
    fn open_writer_keeps_dirty_flag_and_cancelled_operation_poisoned() {
        use crate::storage::files::{FileTable, OpenMode};
        for cancel in [false, true] {
            let mut stream = RefCell::new(Stream::new(run(io::Partition::mbr(disk(), 0)).unwrap()));
            run(async {
                let mut volume = Volume::mount(&mut stream).await.unwrap();
                let mut files = FileTable::<_, 2>::new(&mut volume);
                let a = files.open("/a.bin", OpenMode::CreateNew).await.unwrap();
                let b = files.open("/b.bin", OpenMode::CreateNew).await.unwrap();
                files.write(a, b"first").await.unwrap();
                files.write(b, b"second").await.unwrap();
                files.close(a).await.unwrap();
                files.sync(b).await.unwrap();
                if cancel {
                    let mut op = core::pin::pin!(files.write(b, b"cancel"));
                    assert!(op
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                        .is_pending());
                }
                drop(files);
                assert!(volume.needs_recovery());
                drop(volume);
                assert!(Volume::mount(&mut stream).await.unwrap().needs_recovery());
            });
        }
    }
    #[test]
    fn dirty_volume_mounts_read_only_without_clearing_status() {
        let mut disk = disk();
        let plan = crate::storage::format::Plan::new(disk.sectors).unwrap();
        for copy in 0..2 {
            let lba = u64::from(plan.start)
                + u64::from(plan.reserved)
                + copy * u64::from(plan.fat_sectors);
            let sector = disk.blocks.get_mut(&lba).unwrap();
            let entry = u32::from_le_bytes(sector[4..8].try_into().unwrap());
            sector[4..8].copy_from_slice(&(entry & !(1 << 27)).to_le_bytes());
        }
        let mut stream = RefCell::new(Stream::new(run(io::Partition::mbr(disk, 0)).unwrap()));
        run(async {
            let mut volume = Volume::mount(&mut stream).await.unwrap();
            assert!(volume.needs_recovery());
            assert!(volume.read_dir("/").await.unwrap().next().await.is_none());
            assert!(matches!(
                volume.create_dir("/forbidden").await,
                Err(Error::NeedsRecovery)
            ));
            volume.sync().await.unwrap();
            drop(volume);
            assert!(Volume::mount(&mut stream).await.unwrap().needs_recovery());
        });
    }
    #[test]
    fn dropped_writer_and_write_failure_poison_volume() {
        for inject in [false, true] {
            let disk = disk();
            let fail = disk.fail.clone();
            let mut stream = RefCell::new(Stream::new(run(io::Partition::mbr(disk, 0)).unwrap()));
            let mut v = run(Volume::mount(&mut stream)).unwrap();
            let mut writer = run(v.create_new("/partial.bin")).unwrap();
            if inject {
                fail.set(true);
                assert!(run(writer.write(b"broken")).is_err());
            } else {
                let mut future = core::pin::pin!(writer.write(b"cancel"));
                assert!(future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending());
            }
            drop(writer);
            assert!(v.needs_recovery());
            assert!(matches!(
                run(v.create_dir("/unsafe")),
                Err(Error::NeedsRecovery)
            ));
        }
    }
    #[test]
    fn byte_stream_preserves_neighbors_above_four_gib_and_checks_bounds() {
        let mut stream = RefCell::new(Stream::new(Disk::new(20_000_000)));
        run(async {
            let mut s = stream.borrow_mut();
            let p = (1u64 << 32) + 511;
            s.seek(SeekFrom::Start(p)).await.unwrap();
            s.write_all(b"test").await.unwrap();
            s.seek(SeekFrom::Start(p - 1)).await.unwrap();
            let mut b = [0; 6];
            s.read_exact(&mut b).await.unwrap();
            assert_eq!(&b, b"\0test\0");
            assert!(s.seek(SeekFrom::End(1)).await.is_err());
            assert!(s.seek(SeekFrom::Start(0)).await.is_ok());
            assert!(s.seek(SeekFrom::Current(-1)).await.is_err());
        });
    }
}
