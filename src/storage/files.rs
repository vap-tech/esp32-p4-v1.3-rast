//! Bounded open-file table. One foreground owner executes one operation at a time.
//! Handles survive yields; backend contexts do not borrow the table or volume.
#![allow(clippy::await_holding_refcell_ref)]
use super::{
    asynchronous::BlockDevice,
    volume::{self, Directory, Error, FatFile, Mutation, Volume, PATH_BYTES},
};
use core::sync::atomic::{AtomicU32, Ordering};
use embedded_io_async::{Read, Seek, SeekFrom, Write};
pub const QUANTUM: usize = 32768;
static NEXT_GENERATION: AtomicU32 = AtomicU32::new(1);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileHandle {
    slot: usize,
    generation: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    Read,
    CreateNew,
    Append,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileInfo {
    pub len: u64,
    pub position: u64,
    pub mode: OpenMode,
}
struct Slot {
    generation: u32,
    context: Option<embedded_fatfs::FileContext>,
    path: [u8; PATH_BYTES],
    path_len: usize,
    identity: u64,
    info: FileInfo,
}
impl Slot {
    const EMPTY: Self = Self {
        generation: 0,
        context: None,
        path: [0; PATH_BYTES],
        path_len: 0,
        identity: 0,
        info: FileInfo {
            len: 0,
            position: 0,
            mode: OpenMode::Read,
        },
    };
    fn live(&self) -> bool {
        self.generation != 0
    }
}
pub struct FileTable<'v, 'io, D: BlockDevice, const N: usize> {
    volume: &'v mut Volume<'io, D>,
    slots: [Slot; N],
}
impl<'v, 'io, D: BlockDevice, const N: usize> FileTable<'v, 'io, D, N> {
    pub fn new(volume: &'v mut Volume<'io, D>) -> Self {
        Self {
            volume,
            slots: [const { Slot::EMPTY }; N],
        }
    }
    pub const fn capacity(&self) -> usize {
        N
    }
    pub fn open_count(&self) -> usize {
        self.slots.iter().filter(|s| s.live()).count()
    }
    pub fn volume_capacity(&self) -> u64 {
        self.volume.capacity()
    }
    pub fn needs_recovery(&self) -> bool {
        self.volume.needs_recovery()
    }
    pub fn is_poisoned(&self) -> bool {
        self.volume.fat().poisoned.get() || self.volume.fat().stream.borrow().fat_flush_failed()
    }
    fn writers(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| s.live() && s.info.mode != OpenMode::Read)
            .count()
    }
    fn index(&self, h: FileHandle) -> Result<usize, Error<D::Error>> {
        match self.slots.get(h.slot) {
            Some(s) if s.live() && s.generation == h.generation => Ok(h.slot),
            _ => Err(Error::InvalidHandle),
        }
    }
    pub fn info(&self, h: FileHandle) -> Result<FileInfo, Error<D::Error>> {
        Ok(self.slots[self.index(h)?].info)
    }
    async fn checkpoint(&self, keep_dirty: bool) -> Result<(), Error<D::Error>> {
        let fat = self.volume.fat();
        if keep_dirty {
            fat.fs.flush_preserving_dirty().await?;
        } else {
            fat.fs.flush().await?;
        }
        fat.stream
            .borrow_mut()
            .flush()
            .await
            .map_err(embedded_fatfs::Error::Io)?;
        Ok(())
    }
    pub async fn open(&mut self, p: &str, mode: OpenMode) -> Result<FileHandle, Error<D::Error>> {
        self.volume.readable()?;
        if mode != OpenMode::Read {
            self.volume.writable()?;
        }
        let p = volume::path(p)?;
        let slot = self
            .slots
            .iter()
            .position(|s| !s.live())
            .ok_or(Error::TooManyOpenFiles)?;
        let generation = NEXT_GENERATION
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| Error::GenerationExhausted)?;
        let fat = self.volume.fat();
        // Resolve missing/non-directory parents before any possible mutation.
        if let Some((parent, _)) = p.rsplit_once('/') {
            fat.fs.root_dir().open_dir(parent).await?;
        }
        let guard = Mutation::begin(&fat.poisoned);
        let entry = fat.fs.root_dir().open_meta(p).await;
        let (mut file, len) = match entry {
            Ok(e) => {
                if mode == OpenMode::CreateNew {
                    guard.commit();
                    return Err(Error::AlreadyExists);
                }
                if !e.is_file() {
                    guard.commit();
                    return Err(Error::NotFile);
                }
                if self.slots.iter().any(|s| {
                    s.live()
                        && s.identity == e.entry_position()
                        && (mode != OpenMode::Read || s.info.mode != OpenMode::Read)
                }) {
                    guard.commit();
                    return Err(Error::Busy);
                }
                (e.to_file(), e.len())
            }
            Err(embedded_fatfs::Error::NotFound) if mode != OpenMode::Read => {
                (fat.fs.root_dir().create_file(p).await?, 0)
            }
            Err(e) => {
                if matches!(e, embedded_fatfs::Error::NotFound) {
                    guard.commit();
                }
                return Err(e.into());
            }
        };
        if mode == OpenMode::Append {
            file.seek(SeekFrom::End(0)).await?;
        }
        let context = file.suspend();
        let identity = context.entry_position().ok_or(Error::NotFile)?;
        // Compare disk identity, not spelling: LFN/SFN, case and parent aliases
        // must never allow two writers or stale readable contexts for one file.
        if self.slots.iter().any(|s| {
            s.live()
                && s.identity == identity
                && (mode != OpenMode::Read || s.info.mode != OpenMode::Read)
        }) {
            guard.commit();
            return Err(Error::Busy);
        }
        let mut stored = [0; PATH_BYTES];
        stored[..p.len()].copy_from_slice(p.as_bytes());
        self.slots[slot] = Slot {
            generation,
            context: Some(context),
            path: stored,
            path_len: p.len(),
            identity,
            info: FileInfo {
                len,
                position: if mode == OpenMode::Append { len } else { 0 },
                mode,
            },
        };
        guard.commit();
        Ok(FileHandle { slot, generation })
    }
    pub async fn read(&mut self, h: FileHandle, out: &mut [u8]) -> Result<usize, Error<D::Error>> {
        if self.info(h)?.mode != OpenMode::Read {
            return Err(Error::AccessDenied);
        }
        self.read_inner(h, out).await
    }
    /// Diagnostic readback of the sole newly-created writer; never opens an alias.
    #[cfg(feature = "sd-wide-bench")]
    pub async fn read_written(
        &mut self,
        h: FileHandle,
        out: &mut [u8],
    ) -> Result<usize, Error<D::Error>> {
        if self.info(h)?.mode != OpenMode::CreateNew {
            return Err(Error::AccessDenied);
        }
        self.read_inner(h, out).await
    }
    async fn read_inner(
        &mut self,
        h: FileHandle,
        out: &mut [u8],
    ) -> Result<usize, Error<D::Error>> {
        self.volume.readable()?;
        let i = self.index(h)?;
        let fat = self.volume.fat();
        let guard = Mutation::begin(&fat.poisoned);
        let context = self.slots[i].context.take().ok_or(Error::NeedsRecovery)?;
        let mut file = FatFile::resume_exclusive(context, &fat.fs);
        let limit = out.len().min(QUANTUM);
        let n = file.read(&mut out[..limit]).await?;
        self.slots[i].context = Some(file.suspend());
        self.slots[i].info.position += n as u64;
        guard.commit();
        Ok(n)
    }
    pub async fn write(&mut self, h: FileHandle, bytes: &[u8]) -> Result<usize, Error<D::Error>> {
        self.volume.writable()?;
        let i = self.index(h)?;
        if self.slots[i].info.mode == OpenMode::Read {
            return Err(Error::AccessDenied);
        }
        let n = bytes.len().min(QUANTUM);
        let info = self.slots[i].info;
        if info.position + n as u64 > u32::MAX as u64 {
            return Err(embedded_fatfs::Error::NotEnoughSpace.into());
        }
        let fat = self.volume.fat();
        let guard = Mutation::begin(&fat.poisoned);
        let context = self.slots[i].context.take().ok_or(Error::NeedsRecovery)?;
        let mut file = FatFile::resume_exclusive(context, &fat.fs);
        if info.mode == OpenMode::Append {
            file.seek(SeekFrom::End(0)).await?;
        }
        {
            let _scope = super::asynchronous::Stream::defer_file_fat(fat.stream);
            file.write_all(&bytes[..n]).await?;
        }
        self.slots[i].context = Some(file.suspend());
        self.slots[i].info.position += n as u64;
        self.slots[i].info.len = self.slots[i].info.len.max(self.slots[i].info.position);
        guard.commit();
        Ok(n)
    }
    pub async fn seek(&mut self, h: FileHandle, position: u64) -> Result<(), Error<D::Error>> {
        self.volume.readable()?;
        let i = self.index(h)?;
        if self.slots[i].info.mode == OpenMode::Append {
            return Err(Error::AccessDenied);
        }
        if position > self.slots[i].info.len {
            return Err(Error::InvalidPath);
        }
        let fat = self.volume.fat();
        let guard = Mutation::begin(&fat.poisoned);
        let context = self.slots[i].context.take().ok_or(Error::NeedsRecovery)?;
        let mut file = FatFile::resume_exclusive(context, &fat.fs);
        file.seek(SeekFrom::Start(position)).await?;
        self.slots[i].context = Some(file.suspend());
        self.slots[i].info.position = position;
        guard.commit();
        Ok(())
    }
    pub async fn sync(&mut self, h: FileHandle) -> Result<(), Error<D::Error>> {
        self.volume.readable()?;
        let i = self.index(h)?;
        if self.volume.fat().dirty_at_mount {
            return Ok(());
        }
        let fat = self.volume.fat();
        let guard = Mutation::begin(&fat.poisoned);
        fat.stream
            .borrow_mut()
            .flush_fat()
            .await
            .map_err(embedded_fatfs::Error::Io)?;
        let context = self.slots[i].context.take().ok_or(Error::NeedsRecovery)?;
        self.slots[i].context = Some(FatFile::resume_exclusive(context, &fat.fs).close().await?);
        self.checkpoint(self.writers() != 0).await?;
        guard.commit();
        Ok(())
    }
    pub async fn close(&mut self, h: FileHandle) -> Result<(), Error<D::Error>> {
        self.volume.readable()?;
        let i = self.index(h)?;
        if self.slots[i].info.mode != OpenMode::Read {
            let fat = self.volume.fat();
            let guard = Mutation::begin(&fat.poisoned);
            fat.stream
                .borrow_mut()
                .flush_fat()
                .await
                .map_err(embedded_fatfs::Error::Io)?;
            let context = self.slots[i].context.take().ok_or(Error::NeedsRecovery)?;
            FatFile::resume_exclusive(context, &fat.fs).close().await?;
            self.checkpoint(self.writers() > 1).await?;
            guard.commit();
        }
        self.slots[i] = Slot::EMPTY;
        Ok(())
    }
    /// Only a newly created file can be rolled back by removing it. Append is
    /// preserved on close; cancellation/error never promises rollback of writes.
    pub async fn abort(&mut self, h: FileHandle) -> Result<(), Error<D::Error>> {
        self.volume.writable()?;
        let i = self.index(h)?;
        if self.slots[i].info.mode != OpenMode::CreateNew {
            return Err(Error::AccessDenied);
        }
        let fat = self.volume.fat();
        let guard = Mutation::begin(&fat.poisoned);
        fat.stream
            .borrow_mut()
            .flush_fat()
            .await
            .map_err(embedded_fatfs::Error::Io)?;
        let context = self.slots[i].context.take().ok_or(Error::NeedsRecovery)?;
        // Publish the first cluster before unlink, so remove frees the full chain.
        FatFile::resume_exclusive(context, &fat.fs).close().await?;
        let p = core::str::from_utf8(&self.slots[i].path[..self.slots[i].path_len]).unwrap();
        fat.fs.root_dir().remove(p).await?;
        self.checkpoint(self.writers() > 1).await?;
        self.slots[i] = Slot::EMPTY;
        guard.commit();
        Ok(())
    }
    pub async fn shutdown(&mut self) -> Result<(), Error<D::Error>> {
        for i in 0..N {
            if !self.slots[i].live() {
                continue;
            }
            let h = FileHandle {
                slot: i,
                generation: self.slots[i].generation,
            };
            if self.slots[i].info.mode == OpenMode::CreateNew {
                self.abort(h).await?;
            } else {
                self.close(h).await?;
            }
        }
        Ok(())
    }
    /// Abort incomplete creates, close readers/appends, checkpoint, invalidate clean caches.
    pub async fn release_caches(&mut self) -> Result<(), Error<D::Error>> {
        self.volume.readable()?;
        self.shutdown().await?;
        let fat = self.volume.fat();
        let guard = Mutation::begin(&fat.poisoned);
        if fat.dirty_at_mount {
            // Read-only mount: never clear a pre-existing dirty/I/O-error marker.
            fat.stream
                .borrow_mut()
                .flush()
                .await
                .map_err(embedded_fatfs::Error::Io)?;
        } else {
            self.checkpoint(false).await?;
        }
        fat.stream
            .borrow_mut()
            .invalidate_clean_caches()
            .map_err(embedded_fatfs::Error::Io)?;
        guard.commit();
        Ok(())
    }
    pub async fn read_dir(&mut self, p: &str) -> Result<Directory<'_, 'io, D>, Error<D::Error>> {
        self.volume.read_dir(p).await
    }
    pub async fn read_dir_from(
        &mut self,
        p: &str,
        offset: u64,
    ) -> Result<Directory<'_, 'io, D>, Error<D::Error>> {
        self.volume.read_dir_from(p, offset).await
    }
    pub async fn create_dir(&mut self, p: &str) -> Result<(), Error<D::Error>> {
        self.volume.writable()?;
        let p = volume::path(p)?;
        let fat = self.volume.fat();
        match fat.fs.root_dir().open_meta(p).await {
            Ok(_) => return Err(Error::AlreadyExists),
            Err(embedded_fatfs::Error::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
        let guard = Mutation::begin(&fat.poisoned);
        fat.fs.root_dir().create_dir(p).await?;
        self.checkpoint(self.writers() != 0).await?;
        guard.commit();
        Ok(())
    }
    async fn ensure_unlocked(&self, p: &str) -> Result<(), Error<D::Error>> {
        let fat = self.volume.fat();
        let entry = fat.fs.root_dir().open_meta(p).await?;
        let identity = entry.entry_position();
        if self
            .slots
            .iter()
            .any(|s| s.live() && s.identity == identity)
        {
            return Err(Error::Busy);
        }
        if entry.is_dir() {
            // Resolve parent identities, not textual prefixes: an SFN alias may
            // name the same ancestor as a long or differently cased path.
            for slot in self.slots.iter().filter(|s| s.live()) {
                let path = core::str::from_utf8(&slot.path[..slot.path_len]).unwrap();
                for (offset, c) in path.char_indices() {
                    if c == '/'
                        && fat
                            .fs
                            .root_dir()
                            .open_meta(&path[..offset])
                            .await?
                            .entry_position()
                            == identity
                    {
                        return Err(Error::Busy);
                    }
                }
            }
        }
        Ok(())
    }
    pub async fn remove(&mut self, p: &str) -> Result<(), Error<D::Error>> {
        self.volume.writable()?;
        let p = volume::path(p)?;
        self.ensure_unlocked(p).await?;
        let fat = self.volume.fat();
        let entry = fat.fs.root_dir().open_meta(p).await?;
        if entry.is_dir() && !entry.to_dir().is_empty().await? {
            return Err(embedded_fatfs::Error::DirectoryIsNotEmpty.into());
        }
        let guard = Mutation::begin(&fat.poisoned);
        fat.fs.root_dir().remove(p).await?;
        self.checkpoint(self.writers() != 0).await?;
        guard.commit();
        Ok(())
    }
    pub async fn rename(&mut self, a: &str, b: &str) -> Result<(), Error<D::Error>> {
        self.volume.writable()?;
        let a = volume::path(a)?;
        let b = volume::path(b)?;
        self.ensure_unlocked(a).await?;
        let fat = self.volume.fat();
        let source = fat.fs.root_dir().open_meta(a).await?;
        match fat.fs.root_dir().open_meta(b).await {
            Ok(_) => return Err(Error::AlreadyExists),
            Err(embedded_fatfs::Error::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
        if let Some((parent, _)) = b.rsplit_once('/') {
            fat.fs.root_dir().open_dir(parent).await?;
            if source.is_dir() {
                for (offset, c) in b.char_indices() {
                    if c == '/'
                        && fat
                            .fs
                            .root_dir()
                            .open_meta(&b[..offset])
                            .await?
                            .entry_position()
                            == source.entry_position()
                    {
                        return Err(Error::InvalidPath);
                    }
                }
            }
        }
        let guard = Mutation::begin(&fat.poisoned);
        fat.fs.root_dir().rename(a, &fat.fs.root_dir(), b).await?;
        self.checkpoint(self.writers() != 0).await?;
        guard.commit();
        Ok(())
    }
}
impl<D: BlockDevice, const N: usize> Drop for FileTable<'_, '_, D, N> {
    fn drop(&mut self) {
        if self.writers() != 0 {
            self.volume.fat().poisoned.set(true);
        }
    }
}
