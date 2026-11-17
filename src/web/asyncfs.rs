//! File requests over the async, filesystem-neutral volume boundary.
use super::{chunk, error, headers, mime_type, Error, Transport};
use crate::{
    net::http::{self, Method, Request, Route, Text},
    storage::{
        asynchronous::BlockDevice,
        files::FileTable,
        volume::{Error as FsError, Kind, PATH_BYTES},
    },
};
use core::fmt::Write;
pub fn status<E: core::fmt::Debug>(e: &FsError<E>) -> u16 {
    use embedded_fatfs::Error::*;
    match e {
        FsError::NeedsRecovery | FsError::TooManyOpenFiles | FsError::GenerationExhausted => 503,
        FsError::InvalidHandle | FsError::AccessDenied => 400,
        FsError::Busy => 409,
        FsError::InvalidPath => 400,
        FsError::NotFile => 404,
        FsError::AlreadyExists => 409,
        FsError::Fs(e) => match e {
            NotFound => 404,
            AlreadyExists | DirectoryIsNotEmpty => 409,
            InvalidInput
            | InvalidFileName
            | InvalidFileNameLength
            | UnsupportedFileNameCharacter => 400,
            NotEnoughSpace => 507,
            CorruptedFileSystem => 503,
            _ => 500,
        },
    }
}
pub async fn serve<D: BlockDevice, const N: usize>(
    r: &Request,
    fs: &mut FileTable<'_, '_, D, N>,
    io: &mut impl Transport,
    scratch: &mut [u8],
) -> Result<(), Error> {
    let head = r.method == Method::Head;
    match r.route {
        Route::Index => {
            let file = match fs
                .open("/www/index.html", crate::storage::files::OpenMode::Read)
                .await
            {
                Ok(h) => h,
                Err(e) => return error(io, status(&e), head),
            };
            let result = async {
                let len = fs.info(file).map_err(|_| Error::Storage)?.len;
                headers(
                    io,
                    200,
                    Some(u32::try_from(len).map_err(|_| Error::Storage)?),
                    "text/html; charset=utf-8",
                    "",
                )?;
                if !head {
                    loop {
                        let n = fs.read(file, scratch).await.map_err(|_| Error::Storage)?;
                        if n == 0 {
                            break;
                        }
                        io.write_all(&scratch[..n])?;
                    }
                }
                Ok(())
            }
            .await;
            fs.close(file).await.map_err(|_| Error::Storage)?;
            result?;
        }
        Route::File if matches!(r.method, Method::Mkcol | Method::Delete) => {
            let result = if r.method == Method::Mkcol {
                fs.create_dir(r.path()).await
            } else {
                fs.remove(r.path()).await
            };
            if let Err(e) = result {
                return error(io, status(&e), false);
            }
            headers(
                io,
                if r.method == Method::Mkcol { 201 } else { 204 },
                Some(0),
                "text/plain",
                "",
            )?;
        }
        Route::Rename => {
            if r.argument().contains('/') || r.argument().is_empty() {
                return error(io, 400, false);
            }
            let parent = r.path().rsplit_once('/').map(|p| p.0).unwrap_or("");
            let mut dest = Text::<PATH_BYTES>::new();
            write!(dest, "{}/{}", parent, r.argument()).map_err(|_| Error::Format)?;
            if let Err(e) = fs
                .rename(r.path(), core::str::from_utf8(dest.data()).unwrap())
                .await
            {
                return error(io, status(&e), false);
            }
            headers(io, 204, Some(0), "text/plain", "")?;
        }
        Route::Errors => {
            let cursor = r.argument().parse::<u64>().map_err(|_| Error::Format)?;
            let page = match crate::error_journal::page(fs, cursor).await {
                Ok(p) => p,
                Err(e) => return error(io, status(&e), head),
            };
            let mut body = Text::<256>::new();
            body.write_str("{\"entries\":[")
                .map_err(|_| Error::Format)?;
            for (i, id) in page.ids[..page.len].iter().enumerate() {
                if i != 0 {
                    body.write_char(',').map_err(|_| Error::Format)?;
                }
                write!(body, "\"E{:08X}.JSON\"", id).map_err(|_| Error::Format)?;
            }
            body.write_str("],\"next_cursor\":")
                .map_err(|_| Error::Format)?;
            if let Some(next) = page.next {
                write!(body, "{}", next).map_err(|_| Error::Format)?;
            } else {
                body.write_str("null").map_err(|_| Error::Format)?;
            }
            body.write_char('}').map_err(|_| Error::Format)?;
            super::json(io, body.data())?;
        }
        Route::List => {
            let mut dir = match fs.read_dir(r.path()).await {
                Ok(d) => d,
                Err(e) => return error(io, status(&e), head),
            };
            headers(
                io,
                200,
                None,
                "application/json; charset=utf-8",
                if r.http11 {
                    "Transfer-Encoding: chunked\r\n"
                } else {
                    ""
                },
            )?;
            if !head {
                chunk(io, b"{\"entries\":[", r.http11)?;
                let mut first = true;
                while let Some(e) = dir.next().await {
                    let e = e.map_err(|_| Error::Storage)?;
                    let mut text = Text::<2048>::new();
                    if !first {
                        text.write_char(',').map_err(|_| Error::Format)?;
                    }
                    text.write_str("{\"name\":").map_err(|_| Error::Format)?;
                    http::json_string(&mut text, e.name()).map_err(|_| Error::Format)?;
                    write!(
                        text,
                        ",\"dir\":{},\"size\":{}}}",
                        e.kind == Kind::Directory,
                        e.len
                    )
                    .map_err(|_| Error::Format)?;
                    chunk(io, text.data(), r.http11)?;
                    first = false;
                }
                chunk(io, b"]}", r.http11)?;
                if r.http11 {
                    io.write_all(b"0\r\n\r\n")?;
                }
            }
        }
        _ => return error(io, 501, head),
    }
    io.flush()
}

/// Shared response metadata for cooperatively streamed file transfers.
pub fn file_headers(r: &Request, len: u32, io: &mut impl Transport) -> Result<(), Error> {
    headers(
        io,
        200,
        Some(len),
        mime_type(r.path()),
        if r.route == Route::Static {
            "X-Content-Type-Options: nosniff\r\n"
        } else if r.download {
            "Content-Disposition: attachment\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: sandbox\r\n"
        } else {
            "Content-Disposition: inline\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: sandbox\r\n"
        },
    )
}
pub fn deleted(io: &mut impl Transport) -> Result<(), Error> {
    headers(io, 204, Some(0), "text/plain", "")
}
pub fn created(io: &mut impl Transport) -> Result<(), Error> {
    headers(io, 201, Some(0), "text/plain", "")
}

/// Keep all intermediate file writes sector-aligned; only the final body tail
/// may be shorter. Ring-wrap fragments are joined by the caller, not sent to FS.
pub fn upload_chunk(available: usize, remaining: usize, limit: usize) -> usize {
    let n = available.min(remaining).min(limit);
    if n == remaining {
        n
    } else {
        n / crate::storage::SECTOR_SIZE * crate::storage::SECTOR_SIZE
    }
}
#[cfg(feature = "diagnostics")]
pub fn appended(io: &mut impl Transport) -> Result<(), Error> {
    headers(io, 204, Some(0), "text/plain", "")
}
#[cfg(test)]
mod chunk_tests {
    #[test]
    fn short_rx_and_header_skew_never_force_intermediate_partial_sectors() {
        for (available, remaining, expected) in [
            (0, 10000, 0),
            (511, 10000, 0),
            (6001, 10000, 5632),
            (6144, 10000, 6144),
            (8192, 10000, 8192),
            (808, 808, 808),
            (512, 513, 512),
            (1, 1, 1),
        ] {
            assert_eq!(super::upload_chunk(available, remaining, 8192), expected);
        }
        let mut left = 100_003;
        let mut position = 0;
        while left != 0 {
            let n = super::upload_chunk(6001, left, 8192);
            assert!(n > 0);
            assert_eq!(position % 512, 0);
            position += n;
            left -= n;
        }
        assert_eq!(position, 100_003);
    }
}
