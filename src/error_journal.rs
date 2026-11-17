//! Immutable SD error reports; UNREAD/*.SEEN is a pending, unread report.
//! Publication is close/checkpoint + rename; incomplete .TMP files are not reports.
use crate::{
    net::http::Text,
    storage::{
        asynchronous::BlockDevice,
        files::{FileTable, OpenMode},
        volume::Error,
    },
};
use core::fmt::Write;
pub const DIRECTORY: &str = "/SYSTEM/ERRORS";
pub const UNREAD: &str = "/SYSTEM/ERRORS/UNREAD";
#[derive(Default)]
pub struct Journal {
    next: Option<u32>,
}
fn sequence(name: &str) -> Option<u32> {
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.len() != 9 || !stem.starts_with('E') || !matches!(ext, "JSON" | "TMP" | "SEEN") {
        return None;
    }
    u32::from_str_radix(&stem[1..], 16).ok()
}
impl Journal {
    async fn initialize<D: BlockDevice, const N: usize>(
        &mut self,
        fs: &mut FileTable<'_, '_, D, N>,
    ) -> Result<(), Error<D::Error>> {
        if self.next.is_some() {
            return Ok(());
        }
        for path in ["/SYSTEM", DIRECTORY, UNREAD] {
            match fs.create_dir(path).await {
                Ok(()) | Err(Error::AlreadyExists) => {}
                Err(e) => return Err(e),
            }
        }
        let mut max = 0;
        for path in [DIRECTORY, UNREAD] {
            let mut dir = fs.read_dir(path).await?;
            while let Some(e) = dir.next().await {
                if let Some(id) = sequence(e?.name()) {
                    max = max.max(id);
                }
            }
        }
        self.next = Some(max.checked_add(1).ok_or(Error::GenerationExhausted)?);
        Ok(())
    }
    pub async fn publish<D: BlockDevice, const N: usize>(
        &mut self,
        fs: &mut FileTable<'_, '_, D, N>,
        body: &[u8],
    ) -> Result<u32, Error<D::Error>> {
        self.initialize(fs).await?;
        let id = self.next.unwrap();
        // Reserve IDs even when publication fails; never reuse an interrupted temp file.
        self.next = Some(id.checked_add(1).ok_or(Error::GenerationExhausted)?);
        let mut temp = Text::<64>::new();
        let mut dest = Text::<64>::new();
        write!(temp, "{}/E{:08X}.TMP", DIRECTORY, id).map_err(|_| Error::InvalidPath)?;
        write!(dest, "{}/E{:08X}.JSON", DIRECTORY, id).map_err(|_| Error::InvalidPath)?;
        let temp = core::str::from_utf8(temp.data()).unwrap();
        let dest = core::str::from_utf8(dest.data()).unwrap();
        let h = fs.open(temp, OpenMode::CreateNew).await?;
        let result = async {
            let mut at = 0;
            while at < body.len() {
                let n = fs.write(h, &body[at..]).await?;
                if n == 0 {
                    return Err(Error::Busy);
                }
                at += n;
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            let _ = fs.abort(h).await;
            return Err(e);
        }
        fs.close(h).await?;
        let mut marker = Text::<64>::new();
        write!(marker, "{}/E{:08X}.SEEN", UNREAD, id).map_err(|_| Error::InvalidPath)?;
        let h = fs
            .open(
                core::str::from_utf8(marker.data()).unwrap(),
                OpenMode::CreateNew,
            )
            .await?;
        fs.close(h).await?;
        fs.rename(temp, dest).await?;
        Ok(id)
    }
}
/// Bounded inbox page: never enumerate archived JSON reports.
pub struct Page {
    pub ids: [u32; 5],
    pub len: usize,
    pub next: Option<u64>,
}
pub async fn page<D: BlockDevice, const N: usize>(
    fs: &mut FileTable<'_, '_, D, N>,
    cursor: u64,
) -> Result<Page, Error<D::Error>> {
    let mut out = Page {
        ids: [0; 5],
        len: 0,
        next: None,
    };
    let mut dir = match fs.read_dir_from(UNREAD, cursor).await {
        Ok(d) => d,
        Err(Error::Fs(embedded_fatfs::Error::NotFound)) => return Ok(out),
        Err(e) => return Err(e),
    };
    for _ in 0..64 {
        let Some(e) = dir.next().await else {
            out.next = None;
            return Ok(out);
        };
        let e = e?;
        if e.kind == crate::storage::volume::Kind::File && e.name().ends_with(".SEEN") {
            if let Some(id) = sequence(e.name()) {
                out.ids[out.len] = id;
                out.len += 1;
            }
        }
        out.next = Some(dir.position());
        if out.len == 5 {
            return Ok(out);
        }
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_reserve_interrupted_and_acknowledged_records() {
        assert_eq!(sequence("E0000002A.JSON"), Some(42));
        assert_eq!(sequence("E0000002B.TMP"), Some(43));
        assert_eq!(sequence("E0000002C.SEEN"), Some(44));
        for name in [
            "E123.JSON",
            "EZZZZZZZZ.JSON",
            "E00000001.EXE",
            "X00000001.JSON",
        ] {
            assert_eq!(sequence(name), None);
        }
    }
}
