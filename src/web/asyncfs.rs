//! File requests over the async, filesystem-neutral volume boundary.
use super::{chunk, error, headers, mime_type, Asset, Error, Transport};
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
    asset: &mut impl Asset,
    scratch: &mut [u8],
) -> Result<(), Error> {
    let head = r.method == Method::Head;
    match r.route {
        Route::Index => {
            headers(
                io,
                200,
                Some(asset.len()),
                "text/html; charset=utf-8",
                "Content-Encoding: gzip\r\nVary: Accept-Encoding\r\n",
            )?;
            if !head {
                let mut at = 0;
                while at < asset.len() {
                    let n = asset.read(at, scratch)?;
                    if n == 0 {
                        return Err(Error::Asset);
                    }
                    io.write_all(&scratch[..n])?;
                    at += n as u32;
                }
            }
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
        if r.download {
            "Content-Disposition: attachment\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: sandbox\r\n"
        } else {
            "Content-Disposition: inline\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: sandbox\r\n"
        },
    )
}
pub fn created(io: &mut impl Transport) -> Result<(), Error> {
    headers(io, 201, Some(0), "text/plain", "")
}
