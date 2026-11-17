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
        let (start, end) = fs.fat_region_bytes();
        stream
            .borrow_mut()
            .configure_fat_cache(start, end)
            .map_err(embedded_fatfs::Error::Io)?;
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
        self.fat().dirty_at_mount
            || self.fat().poisoned.get()
            || self.fat().stream.borrow().fat_flush_failed()
    }
    pub(super) fn readable(&self) -> Result<(), Error<D::Error>> {
        if self.fat().poisoned.get() || self.fat().stream.borrow().fat_flush_failed() {
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
        self.read_dir_from(p, 0).await
    }
    pub async fn read_dir_from(
        &mut self,
        p: &str,
        offset: u64,
    ) -> Result<Directory<'_, 'io, D>, Error<D::Error>> {
        self.readable()?;
        let fat = self.fat();
        let root = fat.fs.root_dir();
        let dir = if matches!(p, "" | "/") {
            root
        } else {
            root.open_dir(path(p)?).await?
        };
        Ok(Directory {
            iter: dir.iter_from(offset).await?,
            offset,
            done: false,
        })
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
    offset: u64,
    done: bool,
}
impl<D: BlockDevice> Directory<'_, '_, D> {
    pub fn position(&self) -> u64 {
        self.offset
    }
    pub fn done(&self) -> bool {
        self.done
    }
    pub async fn next(&mut self) -> Option<Result<Entry, Error<D::Error>>> {
        loop {
            let Some(result) = self.iter.next().await else {
                self.done = true;
                return None;
            };
            let e = match result {
                Ok(e) => e,
                Err(e) => return Some(Err(e.into())),
            };
            self.offset = match self.iter.position().await {
                Ok(n) => n,
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
        let _scope = Stream::defer_file_fat(self.fat.stream);
        self.file.as_mut().unwrap().write_all(b).await?;
        self.len += b.len() as u64;
        Ok(())
    }
    pub async fn finish(mut self) -> Result<(), Error<D::Error>> {
        self.fat
            .stream
            .borrow_mut()
            .flush_fat()
            .await
            .map_err(embedded_fatfs::Error::Io)?;
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
        self.fat
            .stream
            .borrow_mut()
            .flush_fat()
            .await
            .map_err(embedded_fatfs::Error::Io)?;
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
    fn directory_cookies_resume_long_names_across_clusters() {
        let mut stream = RefCell::new(Stream::new(run(io::Partition::mbr(disk(), 0)).unwrap()));
        run(async {
            let mut v = Volume::mount(&mut stream).await.unwrap();
            v.create_dir("/photos").await.unwrap();
            for i in 0..150 {
                let path = std::format!("/photos/Фотография длинное имя {i:03}.r565");
                let w = v.create_new(&path).await.unwrap();
                w.finish().await.unwrap();
            }
            assert!(v.read_dir_from("/photos", 1).await.is_err());
            let mut offset = 0;
            let mut names = std::collections::BTreeSet::new();
            loop {
                let mut page = v.read_dir_from("/photos", offset).await.unwrap();
                for _ in 0..16 {
                    let Some(entry) = page.next().await else {
                        break;
                    };
                    let entry = entry.unwrap();
                    assert!(names.insert(std::string::String::from(entry.name())));
                }
                offset = page.position();
                assert_eq!(offset % 32, 0);
                if page.done() {
                    break;
                }
            }
            assert_eq!(names.len(), 150);
            assert!(names.contains("Фотография длинное имя 149.r565"));
        });
    }
    #[test]
    fn live_cleanup_aborts_partial_create_preserves_append_and_reuses_volume() {
        use crate::storage::files::{FileTable, OpenMode};
        let mut stream = RefCell::new(Stream::new(run(io::Partition::mbr(disk(), 0)).unwrap()));
        run(async {
            let mut volume = Volume::mount(&mut stream).await.unwrap();
            let mut files = FileTable::<_, 5>::new(&mut volume);
            let kept = files.open("/kept.txt", OpenMode::CreateNew).await.unwrap();
            files.write(kept, b"kept").await.unwrap();
            files.close(kept).await.unwrap();
            let partial = files
                .open("/partial.bin", OpenMode::CreateNew)
                .await
                .unwrap();
            files.write(partial, &[9; 8192]).await.unwrap();
            let append = files.open("/events.log", OpenMode::Append).await.unwrap();
            files.write(append, b"event\n").await.unwrap();
            let read = files.open("/kept.txt", OpenMode::Read).await.unwrap();
            files.release_caches().await.unwrap();
            assert_eq!(files.open_count(), 0);
            assert!(!files.is_poisoned());
            assert!(files.info(partial).is_err());
            assert!(files.info(read).is_err());
            assert!(files.open("/partial.bin", OpenMode::Read).await.is_err());
            for (path, expected) in [
                ("/kept.txt", &b"kept"[..]),
                ("/events.log", &b"event\n"[..]),
            ] {
                let handle = files.open(path, OpenMode::Read).await.unwrap();
                let mut data = [0; 16];
                let n = files.read(handle, &mut data).await.unwrap();
                assert_eq!(&data[..n], expected);
                files.close(handle).await.unwrap();
            }
            let again = files.open("/again.txt", OpenMode::CreateNew).await.unwrap();
            files.write(again, b"new").await.unwrap();
            files.close(again).await.unwrap();
            files.release_caches().await.unwrap();
            drop(files);
            drop(volume);
            let mut remount = Volume::mount(&mut stream).await.unwrap();
            assert!(!remount.needs_recovery());
            assert_eq!(remount.open("/again.txt").await.unwrap().len(), 3);
        });
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
            let block = [0xa7; crate::storage::files::QUANTUM * 2];
            for _ in 0..12 {
                assert_eq!(
                    files.write(a, &block).await.unwrap(),
                    crate::storage::files::QUANTUM
                );
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
            assert_eq!(
                files.info(a).unwrap().len,
                12 * crate::storage::files::QUANTUM as u64
            );
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
            {
                let mut files = crate::storage::files::FileTable::<_, 5>::new(&mut volume);
                files.release_caches().await.unwrap();
                assert!(files.needs_recovery());
            }
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
    #[derive(Clone, Debug)]
    enum AuditEvent {
        Write(u64, std::vec::Vec<u8>),
        Sync,
    }
    struct AuditDisk {
        disk: Disk,
        events: Rc<RefCell<std::vec::Vec<AuditEvent>>>,
        fail_lba: Rc<Cell<Option<u64>>>,
    }
    impl BlockDevice for AuditDisk {
        type Error = Fault;
        fn sectors(&self) -> u64 {
            self.disk.sectors
        }
        fn max_transfer_bytes(&self) -> usize {
            8192
        }
        async fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), Fault> {
            crate::storage::BlockDevice::read(&mut self.disk, lba, out)
        }
        async fn write(&mut self, lba: u64, bytes: &[u8]) -> Result<(), Fault> {
            if self
                .fail_lba
                .get()
                .is_some_and(|n| n >= lba && n < lba + (bytes.len() / 512) as u64)
            {
                return Err(Fault);
            }
            crate::storage::BlockDevice::write(&mut self.disk, lba, bytes)?;
            self.events
                .borrow_mut()
                .push(AuditEvent::Write(lba, bytes.to_vec()));
            Ok(())
        }
        async fn sync(&mut self) -> Result<(), Fault> {
            self.events.borrow_mut().push(AuditEvent::Sync);
            Ok(())
        }
    }
    #[test]
    fn bounded_delete_batches_fat_interleaves_io_and_preserves_dirty_until_done() {
        use crate::storage::files::{FileTable, OpenMode};
        let plan = crate::storage::format::Plan::new(131072).unwrap();
        let fat1 = u64::from(plan.start + u32::from(plan.reserved));
        let fat_end = fat1 + u64::from(plan.fat_sectors) * 2;
        let events = Rc::new(RefCell::new(std::vec::Vec::new()));
        let disk = AuditDisk {
            disk: disk(),
            events: events.clone(),
            fail_lba: Rc::new(Cell::new(None)),
        };
        let mut stream = RefCell::new(Stream::new(run(io::Partition::mbr(disk, 0)).unwrap()));
        let mut volume = run(Volume::mount(&mut stream)).unwrap();
        let cluster = volume.fat().fs.cluster_size() as usize;
        let mut files = FileTable::<_, 5>::new(&mut volume);
        let h = run(files.open("/large.bin", OpenMode::CreateNew)).unwrap();
        let block = std::vec![0x5a; cluster];
        for _ in 0..385 {
            run(files.write(h, &block)).unwrap();
        }
        assert!(matches!(
            run(files.begin_remove("/large.bin")),
            Err(Error::Busy)
        ));
        run(files.close(h)).unwrap();
        let deletion_start = events.borrow().len();
        run(files.begin_remove("/large.bin")).unwrap();
        assert!(files.removing());
        assert!(matches!(
            run(files.open("/large.bin", OpenMode::Read)),
            Err(Error::Fs(embedded_fatfs::Error::NotFound))
        ));
        assert!(!run(files.remove_step()).unwrap());
        assert!(matches!(
            run(files.begin_remove("/other.bin")),
            Err(Error::Busy)
        ));
        // Reusing the detached name is safe; its new clusters must not be reclaimed.
        let h = run(files.open("/large.bin", OpenMode::CreateNew)).unwrap();
        run(files.write(h, b"new file")).unwrap();
        run(files.close(h)).unwrap();
        // A close/checkpoint during deletion must not mark an incomplete volume clean.
        let mut snapshot = self::disk();
        for event in events.borrow().iter() {
            if let AuditEvent::Write(lba, bytes) = event {
                crate::storage::BlockDevice::write(&mut snapshot, *lba, bytes).unwrap();
            }
        }
        let mut copy = RefCell::new(Stream::new(run(io::Partition::mbr(snapshot, 0)).unwrap()));
        let mounted = run(Volume::mount(&mut copy)).unwrap();
        assert!(mounted.needs_recovery());
        drop(mounted);
        let mut steps = 1;
        while files.removing() {
            run(files.remove_step()).unwrap();
            steps += 1;
            let h = run(files.open("/large.bin", OpenMode::Read)).unwrap();
            let mut data = [0; 8];
            assert_eq!(run(files.read(h, &mut data)).unwrap(), 8);
            assert_eq!(&data, b"new file");
            run(files.close(h)).unwrap();
        }
        assert_eq!(steps, 4);
        let writes = events.borrow()[deletion_start..]
            .iter()
            .filter(|e| matches!(e, AuditEvent::Write(lba,_) if *lba>=fat1 && *lba<fat_end))
            .count();
        assert!(writes < 32, "batched FAT writes: {writes}");
        drop(files);
        drop(volume);
        let mut volume = run(Volume::mount(&mut stream)).unwrap();
        assert!(!volume.needs_recovery());
        assert_eq!(run(volume.open("/large.bin")).unwrap().len(), 8);
    }
    #[test]
    fn delete_failure_or_abandoned_job_keeps_volume_dirty() {
        use crate::storage::files::{FileTable, OpenMode};
        for fail_write in [false, true] {
            let plan = crate::storage::format::Plan::new(131072).unwrap();
            let mirror = u64::from(plan.start + u32::from(plan.reserved) + plan.fat_sectors);
            let fail = Rc::new(Cell::new(None));
            let disk = AuditDisk {
                disk: disk(),
                events: Rc::new(RefCell::new(std::vec::Vec::new())),
                fail_lba: fail.clone(),
            };
            let mut stream = RefCell::new(Stream::new(run(io::Partition::mbr(disk, 0)).unwrap()));
            let mut volume = run(Volume::mount(&mut stream)).unwrap();
            let mut files = FileTable::<_, 5>::new(&mut volume);
            let h = run(files.open("/doomed.bin", OpenMode::CreateNew)).unwrap();
            run(files.write(h, &[0x73; 8192])).unwrap();
            run(files.close(h)).unwrap();
            run(files.begin_remove("/doomed.bin")).unwrap();
            if fail_write {
                fail.set(Some(mirror));
                assert!(run(files.remove_step()).is_err());
                assert!(files.is_poisoned());
                assert!(matches!(
                    run(files.open("/unsafe.bin", OpenMode::CreateNew)),
                    Err(Error::NeedsRecovery)
                ));
            }
            drop(files);
            assert!(volume.needs_recovery());
            drop(volume);
            fail.set(None);
            // Simulate a cold reset: discard RAM caches and their failed-flush latch.
            let partition = stream.into_inner().device;
            let mut stream = RefCell::new(Stream::new(partition));
            let mut mounted = run(Volume::mount(&mut stream)).unwrap();
            assert!(mounted.needs_recovery());
            assert!(matches!(
                run(mounted.open("/doomed.bin")),
                Err(Error::Fs(embedded_fatfs::Error::NotFound))
            ));
        }
    }
    #[test]
    fn fat_checkpoint_precedes_directory_publication_and_clean_marker() {
        use crate::storage::files::{FileTable, OpenMode};
        for fail_mirror in [false, true] {
            let plan = crate::storage::format::Plan::new(131072).unwrap();
            let fat1 = u64::from(plan.start) + u64::from(plan.reserved);
            let fat2 = fat1 + u64::from(plan.fat_sectors);
            let root = u64::from(plan.start + plan.data_start);
            let log = Rc::new(RefCell::new(std::vec::Vec::new()));
            let fail = Rc::new(Cell::new(None));
            let disk = AuditDisk {
                disk: disk(),
                events: log.clone(),
                fail_lba: fail.clone(),
            };
            let mut stream = RefCell::new(Stream::new(run(io::Partition::mbr(disk, 0)).unwrap()));
            let mut volume = run(Volume::mount(&mut stream)).unwrap();
            let mut files = FileTable::<_, 2>::new(&mut volume);
            let h = run(files.open("/ORDER.BIN", OpenMode::CreateNew)).unwrap();
            log.borrow_mut().clear();
            assert_eq!(run(files.write(h, &[0x3c; 3072])).unwrap(), 3072);
            assert!(!log
                .borrow()
                .iter()
                .any(|e| matches!(e, AuditEvent::Write(lba,_) if *lba==fat1 || *lba==fat2)));
            if fail_mirror {
                fail.set(Some(fat2));
            }
            let result = run(files.sync(h));
            if fail_mirror {
                assert!(result.is_err());
                assert!(files.is_poisoned());
                assert!(log
                    .borrow()
                    .iter()
                    .any(|e| matches!(e, AuditEvent::Write(lba,_) if *lba==fat1)));
                assert!(!log
                    .borrow()
                    .iter()
                    .any(|e| matches!(e, AuditEvent::Write(lba,_) if *lba==root)));
                drop(files);
                drop(volume);
                assert!(stream.borrow().has_dirty_fat());
                assert!(stream.borrow().fat_flush_failed());
            } else {
                result.unwrap();
                let events = log.borrow();
                let first = events
                    .iter()
                    .position(|e| matches!(e, AuditEvent::Write(lba,_) if *lba==fat1))
                    .unwrap();
                let second = events
                    .iter()
                    .position(|e| matches!(e, AuditEvent::Write(lba,_) if *lba==fat2))
                    .unwrap();
                let publish = events
                    .iter()
                    .position(|e| matches!(e, AuditEvent::Write(lba,_) if *lba==root))
                    .unwrap();
                assert!(first < publish && second < publish);
                assert!(events[second + 1..publish]
                    .iter()
                    .any(|e| matches!(e, AuditEvent::Sync)));
                assert!(!events.iter().any(|e| matches!(e, AuditEvent::Write(lba,b) if *lba==u64::from(plan.start) && b[0x41]&1==0)));
                drop(events);
                run(files.close(h)).unwrap();
                let events = log.borrow();
                let clean = events.iter().rposition(|e| matches!(e, AuditEvent::Write(lba,b) if *lba==u64::from(plan.start)+6 && b[0x41]&1==0)).unwrap();
                assert!(events[clean + 1..]
                    .iter()
                    .all(|e| matches!(e, AuditEvent::Sync)));
                drop(events);
                drop(files);
                drop(volume);
                assert!(!stream.borrow().has_dirty_fat());
                let mut remount = run(Volume::mount(&mut stream)).unwrap();
                assert!(!remount.needs_recovery());
                let mut file = run(remount.open("/ORDER.BIN")).unwrap();
                let mut out = [0; 3072];
                run(async {
                    let mut at = 0;
                    while at < out.len() {
                        at += file.read(&mut out[at..]).await.unwrap();
                    }
                });
                assert_eq!(out, [0x3c; 3072]);
            }
        }
    }
    #[test]
    fn cached_fat12_and_fat16_cross_sector_chains_remount_correctly() {
        use embedded_fatfs::{FatType, FormatVolumeOptions};
        for (kind, sectors) in [(FatType::Fat12, 4096), (FatType::Fat16, 16384)] {
            let mut stream = RefCell::new(Stream::new(Disk::new(sectors)));
            run(embedded_fatfs::format_volume(
                &mut *stream.borrow_mut(),
                FormatVolumeOptions::new()
                    .fat_type(kind)
                    .bytes_per_cluster(512)
                    .total_sectors(sectors as u32),
            ))
            .unwrap();
            {
                let mut v = run(Volume::mount(&mut stream)).unwrap();
                let mut w = run(v.create_new("/CHAIN.BIN")).unwrap();
                for i in 0..400 {
                    run(w.write(&[i as u8; 512])).unwrap();
                }
                run(w.finish()).unwrap();
            }
            let mut v = run(Volume::mount(&mut stream)).unwrap();
            assert!(!v.needs_recovery());
            let mut file = run(v.open("/CHAIN.BIN")).unwrap();
            for i in 0..400 {
                let mut out = [0; 512];
                let mut at = 0;
                while at < 512 {
                    at += run(file.read(&mut out[at..])).unwrap();
                }
                assert_eq!(out, [i as u8; 512]);
            }
        }
    }
    #[test]
    fn error_journal_publish_seen_and_reboot_reserve_incomplete_ids() {
        use crate::{
            error_journal::Journal,
            storage::files::{FileTable, OpenMode},
        };
        let mut stream = RefCell::new(Stream::new(run(io::Partition::mbr(disk(), 0)).unwrap()));
        run(async {
            {
                let mut volume = Volume::mount(&mut stream).await.unwrap();
                let mut fs = FileTable::<_, 5>::new(&mut volume);
                let mut journal = Journal::default();
                assert_eq!(
                    journal
                        .publish(&mut fs, b"{\"title\":\"test\"}")
                        .await
                        .unwrap(),
                    1
                );
                let page = crate::error_journal::page(&mut fs, 0).await.unwrap();
                assert_eq!(&page.ids[..page.len], &[1]);
                fs.begin_remove("/SYSTEM/ERRORS/UNREAD/E00000001.SEEN")
                    .await
                    .unwrap();
                while !fs.remove_step().await.unwrap() {}
                let h = fs
                    .open("/SYSTEM/ERRORS/E00000009.TMP", OpenMode::CreateNew)
                    .await
                    .unwrap();
                fs.write(h, b"incomplete").await.unwrap();
                fs.close(h).await.unwrap();
                fs.shutdown().await.unwrap();
            }
            let mut volume = Volume::mount(&mut stream).await.unwrap();
            assert!(!volume.needs_recovery());
            let mut fs = FileTable::<_, 5>::new(&mut volume);
            let mut journal = Journal::default();
            assert_eq!(
                journal
                    .publish(&mut fs, b"{\"title\":\"next\"}")
                    .await
                    .unwrap(),
                10
            );
            let h = fs
                .open("/SYSTEM/ERRORS/E00000001.JSON", OpenMode::Read)
                .await
                .unwrap();
            let mut bytes = [0u8; 32];
            let n = fs.read(h, &mut bytes).await.unwrap();
            assert_eq!(&bytes[..n], b"{\"title\":\"test\"}");
            fs.close(h).await.unwrap();
            let page = crate::error_journal::page(&mut fs, 0).await.unwrap();
            assert_eq!(&page.ids[..page.len], &[10]);
            assert!(fs
                .open("/SYSTEM/ERRORS/E00000009.JSON", OpenMode::Read)
                .await
                .is_err());
            // A large archive must not be returned or filtered by the browser.
            for id in 100..1100 {
                let path = std::format!("/SYSTEM/ERRORS/E{id:08X}.JSON");
                let h = fs.open(&path, OpenMode::CreateNew).await.unwrap();
                fs.close(h).await.unwrap();
            }
            for id in 1100..1106 {
                let path = std::format!("/SYSTEM/ERRORS/UNREAD/E{id:08X}.SEEN");
                let h = fs.open(&path, OpenMode::CreateNew).await.unwrap();
                fs.close(h).await.unwrap();
            }
            let first = crate::error_journal::page(&mut fs, 0).await.unwrap();
            assert_eq!(&first.ids[..first.len], &[10, 1100, 1101, 1102, 1103]);
            let second = crate::error_journal::page(&mut fs, first.next.unwrap())
                .await
                .unwrap();
            assert_eq!(&second.ids[..second.len], &[1104, 1105]);
            assert_eq!(second.next, None);
            assert_eq!(fs.open_count(), 0);
            fs.shutdown().await.unwrap();
        });
    }
}
