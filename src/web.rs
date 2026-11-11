//! Synchronous file request service over a polled transport. No file-sized buffer.
use crate::{
    net::http::{self, Method, Request, Route, Text},
    storage::{self, fat32::Fat32, BlockDevice},
};
use core::fmt::Write;
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    CacheConfiguration,
    Link,
    Transport,
    Storage,
    Format,
    Asset,
}
pub trait Transport {
    fn read(&mut self, out: &mut [u8]) -> Result<usize, Error>;
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error>;
    fn flush(&mut self) -> Result<(), Error>;
}
pub trait Asset {
    fn len(&self) -> u32;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn read(&mut self, offset: u32, out: &mut [u8]) -> Result<usize, Error>;
}
fn status<E>(error: &storage::Error<E>) -> u16 {
    use storage::Error::*;
    match error {
        NotFound | NotFile | NotDirectory => 404,
        InvalidName | Unsupported(_) | InvalidOffset => 400,
        AlreadyExists => 409,
        NoSpace | DirectoryFull => 507,
        NeedsRecovery => 503,
        _ => 500,
    }
}
fn headers(
    io: &mut impl Transport,
    code: u16,
    len: Option<u32>,
    kind: &str,
    extra: &str,
) -> Result<(), Error> {
    let mut text = Text::<512>::new();
    write!(text,"HTTP/1.1 {code} {}\r\nConnection: close\r\nContent-Type: {kind}\r\nCache-Control: no-store\r\n",http::reason(code)).map_err(|_|Error::Format)?;
    if let Some(n) = len {
        write!(text, "Content-Length: {n}\r\n")
    } else {
        Ok(())
    }
    .map_err(|_| Error::Format)?;
    write!(text, "{extra}\r\n").map_err(|_| Error::Format)?;
    io.write_all(text.data())
}
pub fn error(io: &mut impl Transport, code: u16, head: bool) -> Result<(), Error> {
    let body = http::reason(code).as_bytes();
    headers(
        io,
        code,
        Some(body.len() as u32),
        "text/plain; charset=utf-8",
        "",
    )?;
    if !head {
        io.write_all(body)?;
    }
    io.flush()
}
fn chunk(io: &mut impl Transport, data: &[u8], chunked: bool) -> Result<(), Error> {
    if !chunked {
        return io.write_all(data);
    }
    let mut text = Text::<16>::new();
    write!(text, "{:x}\r\n", data.len()).map_err(|_| Error::Format)?;
    io.write_all(text.data())?;
    io.write_all(data)?;
    io.write_all(b"\r\n")
}
/// One borrowed file/writer at a time. Clean disconnect aborts unpublished PUT;
/// storage failures intentionally leave dirty state for external recovery.
pub fn serve<D: BlockDevice>(
    request: &Request,
    fs: &mut Fat32<D>,
    io: &mut impl Transport,
    asset: &mut impl Asset,
    scratch: &mut [u8],
) -> Result<(), Error> {
    let head = request.method == Method::Head;
    match request.route {
        Route::Index => {
            headers(
                io,
                200,
                Some(asset.len()),
                "text/html; charset=utf-8",
                "Content-Encoding: gzip\r\nVary: Accept-Encoding\r\n",
            )?;
            if !head {
                let mut offset = 0;
                while offset < asset.len() {
                    let n = asset.read(offset, scratch)?;
                    if n == 0 || n as u32 > asset.len() - offset {
                        return Err(Error::Asset);
                    }
                    io.write_all(&scratch[..n])?;
                    offset += n as u32;
                }
            }
        }
        Route::File if request.method == Method::Put => {
            let mut writer = match fs.create_stream(request.path()) {
                Ok(w) => w,
                Err(e) => return error(io, status(&e), false),
            };
            if request.expect {
                if let Err(e) = io
                    .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                    .and_then(|_| io.flush())
                {
                    let _ = writer.abort();
                    return Err(e);
                }
            }
            let mut remaining = request.length;
            while remaining != 0 {
                let want = scratch.len().min(remaining as usize);
                let n = match io.read(&mut scratch[..want]) {
                    Ok(n) if n > 0 => n,
                    _ => {
                        let _ = writer.abort();
                        return Err(Error::Transport);
                    }
                };
                if let Err(e) = writer.write(&scratch[..n]) {
                    drop(writer);
                    return error(io, status(&e), false);
                }
                remaining -= n as u32;
            }
            if let Err(e) = writer.finish() {
                return error(io, status(&e), false);
            }
            headers(io, 201, Some(0), "text/plain", "")?;
        }
        Route::File => {
            let mut file = match fs.open(request.path()) {
                Ok(f) => f,
                Err(e) => return error(io, status(&e), head),
            };
            headers(
                io,
                200,
                Some(file.len()),
                "application/octet-stream",
                "Content-Disposition: attachment\r\n",
            )?;
            if !head {
                loop {
                    let n = file.read(scratch).map_err(|_| Error::Storage)?;
                    if n == 0 {
                        break;
                    }
                    io.write_all(&scratch[..n])?;
                }
            }
        }
        Route::List => {
            // Resolve before publishing a 200 response; enumerate once, then stream.
            if let Err(e) = fs.read_dir(request.path(), |_| false) {
                return error(io, status(&e), head);
            }
            headers(
                io,
                200,
                None,
                "application/json; charset=utf-8",
                if request.http11 {
                    "Transfer-Encoding: chunked\r\n"
                } else {
                    ""
                },
            )?;
            if !head {
                chunk(io, b"{\"entries\":[", request.http11)?;
                let mut first = true;
                let mut failed = None;
                let result = fs.read_dir(request.path(), |entry| {
                    let mut name = Text::<16>::new();
                    if write!(name, "{}", entry.name).is_err() {
                        failed = Some(Error::Format);
                        return false;
                    }
                    if matches!(name.data(), b"." | b"..") {
                        return true;
                    }
                    let mut text = Text::<256>::new();
                    let result = (|| {
                        if !first {
                            text.write_char(',')?;
                        }
                        text.write_str("{\"name\":")?;
                        http::json_string(
                            &mut text,
                            core::str::from_utf8(name.data()).map_err(|_| core::fmt::Error)?,
                        )?;
                        write!(
                            text,
                            ",\"dir\":{},\"size\":{}}}",
                            entry.is_directory(),
                            entry.size
                        )
                    })();
                    if result.is_err() {
                        failed = Some(Error::Format);
                        return false;
                    }
                    first = false;
                    if let Err(e) = chunk(io, text.data(), request.http11) {
                        failed = Some(e);
                        return false;
                    }
                    true
                });
                if let Some(e) = failed {
                    return Err(e);
                }
                result.map_err(|_| Error::Storage)?;
                chunk(io, b"]}", request.http11)?;
                if request.http11 {
                    io.write_all(b"0\r\n\r\n")?;
                }
            }
        }
    }
    io.flush()
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{collections::BTreeMap, vec::Vec};
    struct Disk(BTreeMap<u64, [u8; 512]>);
    impl BlockDevice for Disk {
        type Error = ();
        fn sectors(&self) -> u64 {
            100_000
        }
        fn max_transfer_bytes(&self) -> usize {
            8192
        }
        fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), ()> {
            for (i, s) in out.as_chunks_mut::<512>().0.iter_mut().enumerate() {
                *s = *self.0.get(&(lba + i as u64)).unwrap_or(&[0; 512]);
            }
            Ok(())
        }
        fn write(&mut self, lba: u64, data: &[u8]) -> Result<(), ()> {
            for (i, s) in data.as_chunks::<512>().0.iter().enumerate() {
                self.0.insert(lba + i as u64, *s);
            }
            Ok(())
        }
    }
    struct Wire {
        input: Vec<u8>,
        at: usize,
        out: Vec<u8>,
    }
    impl Wire {
        fn new(input: &[u8]) -> Self {
            Self {
                input: input.to_vec(),
                at: 0,
                out: Vec::new(),
            }
        }
    }
    impl Transport for Wire {
        fn read(&mut self, out: &mut [u8]) -> Result<usize, Error> {
            let n = out.len().min(17).min(self.input.len() - self.at);
            out[..n].copy_from_slice(&self.input[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        }
        fn write_all(&mut self, b: &[u8]) -> Result<(), Error> {
            self.out.extend_from_slice(b);
            Ok(())
        }
        fn flush(&mut self) -> Result<(), Error> {
            Ok(())
        }
    }
    struct Page;
    impl Asset for Page {
        fn len(&self) -> u32 {
            4
        }
        fn read(&mut self, at: u32, out: &mut [u8]) -> Result<usize, Error> {
            let n = out.len().min(4 - at as usize);
            out[..n].copy_from_slice(&b"PAGE"[at as usize..at as usize + n]);
            Ok(n)
        }
    }
    fn request(method: &str, path: &str, length: usize) -> Request {
        let mut t = Text::<256>::new();
        write!(
            t,
            "{method} {path} HTTP/1.1\r\nHost: p4\r\nContent-Length: {length}\r\n\r\n"
        )
        .unwrap();
        http::parse(t.data()).unwrap()
    }
    #[test]
    fn http_stream_put_abort_conflict_get_head_and_list() {
        let mut disk = Disk(BTreeMap::new());
        let mut scratch = [0; 8192];
        storage::format::quick_format(&mut disk, &mut scratch, 123, |_, _| {}).unwrap();
        let mut fs = Fat32::mount(storage::Partition::mbr(disk, 0).unwrap()).unwrap();
        let data: Vec<u8> = (0..12_345).map(|n| (n * 37) as u8).collect();
        let mut wire = Wire::new(&data);
        serve(
            &request("PUT", "/fs/GOOD.BIN", data.len()),
            &mut fs,
            &mut wire,
            &mut Page,
            &mut scratch,
        )
        .unwrap();
        assert!(wire.out.starts_with(b"HTTP/1.1 201"));
        let mut wire = Wire::new(&data);
        serve(
            &request("PUT", "/fs/GOOD.BIN", data.len()),
            &mut fs,
            &mut wire,
            &mut Page,
            &mut scratch,
        )
        .unwrap();
        assert!(wire.out.starts_with(b"HTTP/1.1 409"));
        assert_eq!(wire.at, 0);
        let mut wire = Wire::new(&data[..1000]);
        assert_eq!(
            serve(
                &request("PUT", "/fs/ABORT.BIN", data.len()),
                &mut fs,
                &mut wire,
                &mut Page,
                &mut scratch
            ),
            Err(Error::Transport)
        );
        assert!(matches!(
            fs.open("ABORT.BIN"),
            Err(storage::Error::NotFound)
        ));
        fs.create_new("AFTER.TXT", b"ok").unwrap();
        let mut wire = Wire::new(&[]);
        serve(
            &request("GET", "/fs/GOOD.BIN", 0),
            &mut fs,
            &mut wire,
            &mut Page,
            &mut scratch,
        )
        .unwrap();
        let start = http::header_end(&wire.out).unwrap();
        assert_eq!(&wire.out[start..], data.as_slice());
        let mut wire = Wire::new(&[]);
        serve(
            &request("HEAD", "/fs/GOOD.BIN", 0),
            &mut fs,
            &mut wire,
            &mut Page,
            &mut scratch,
        )
        .unwrap();
        assert_eq!(http::header_end(&wire.out), Some(wire.out.len()));
        let mut wire = Wire::new(&[]);
        serve(
            &request("GET", "/api/list?path=%2F", 0),
            &mut fs,
            &mut wire,
            &mut Page,
            &mut scratch,
        )
        .unwrap();
        assert!(wire.out.windows(8).any(|p| p == b"GOOD.BIN"));
        assert!(!wire.out.windows(9).any(|p| p == b"ABORT.BIN"));
        assert!(wire.out.ends_with(b"0\r\n\r\n"));
        assert!(wire
            .out
            .windows(b"Transfer-Encoding: chunked".len())
            .any(|p| p == b"Transfer-Encoding: chunked"));
        let mut old = request("GET", "/api/list?path=%2F", 0);
        old.http11 = false;
        let mut wire = Wire::new(&[]);
        serve(&old, &mut fs, &mut wire, &mut Page, &mut scratch).unwrap();
        let start = http::header_end(&wire.out).unwrap();
        assert!(wire.out[start..].starts_with(b"{\"entries\":["));
        assert!(wire.out.ends_with(b"]}"));
        assert!(!wire.out[..start]
            .windows(17)
            .any(|p| p == b"Transfer-Encoding"));
    }
}
