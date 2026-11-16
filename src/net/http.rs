//! Small strict HTTP/1 parser. One request per connection; PUT has a known length.
//! Transfer-Encoding is rejected, never guessed from connection closure.
use core::fmt;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Put,
    Post,
    Delete,
    Mkcol,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub path: [u8; crate::storage::volume::PATH_BYTES],
    pub path_len: usize,
    pub length: u32,
    pub expect: bool,
    pub http11: bool,
    pub route: Route,
    pub argument: [u8; crate::storage::volume::PATH_BYTES],
    pub argument_len: usize,
    pub download: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Index,
    List,
    File,
    Static,
    Rename,
    Clipboard,
    State,
    Brightness,
    Release,
    NetTest,
    PhotoStatus,
    PhotoStart,
    PhotoStop,
}
impl Request {
    pub fn argument(&self) -> &str {
        core::str::from_utf8(&self.argument[..self.argument_len]).unwrap()
    }
    pub fn path(&self) -> &str {
        core::str::from_utf8(&self.path[..self.path_len]).unwrap()
    }
}
pub fn header_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|p| p == b"\r\n\r\n")
        .map(|n| n + 4)
}
pub fn parse(bytes: &[u8]) -> Result<Request, u16> {
    if bytes.len() > 2048 {
        return Err(431);
    }
    if header_end(bytes) != Some(bytes.len()) {
        return Err(400);
    }
    if !bytes.is_ascii() {
        return Err(400);
    }
    let text = core::str::from_utf8(bytes).map_err(|_| 400u16)?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next().ok_or(400u16)?.split(' ');
    let method = match first.next() {
        Some("GET") => Method::Get,
        Some("HEAD") => Method::Head,
        Some("PUT") => Method::Put,
        Some("POST") => Method::Post,
        Some("DELETE") => Method::Delete,
        Some("MKCOL") => Method::Mkcol,
        _ => return Err(405),
    };
    let target = first.next().ok_or(400u16)?;
    let version = first.next().ok_or(400u16)?;
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") || first.next().is_some() {
        return Err(400);
    }
    let mut length = None;
    let mut host = false;
    let mut expect = false;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').ok_or(400u16)?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        {
            return Err(400);
        }
        let value = value.trim_matches([' ', '\t']);
        if value.bytes().any(|b| b < 32 && b != 9 || b == 127) {
            return Err(400);
        }
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(400);
            }
            let n = value.parse::<u64>().map_err(|_| 413u16)?;
            length = Some(u32::try_from(n).map_err(|_| 413u16)?);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(501);
        } else if name.eq_ignore_ascii_case("content-encoding")
            && !value.eq_ignore_ascii_case("identity")
        {
            return Err(415);
        } else if name.eq_ignore_ascii_case("host") {
            if host || value.is_empty() {
                return Err(400);
            }
            host = true;
        } else if name.eq_ignore_ascii_case("expect") {
            if !value.eq_ignore_ascii_case("100-continue") {
                return Err(417);
            }
            expect = true;
        }
    }
    if version == "HTTP/1.1" && !host {
        return Err(400);
    }
    if method == Method::Put && length.is_none() {
        return Err(411);
    }
    if !matches!(method, Method::Put | Method::Post) && length.unwrap_or(0) != 0 {
        return Err(400);
    }
    let (target, query) = target.split_once('?').unwrap_or((target, ""));
    let mut argument = "";
    let mut download = false;
    let (route, path) = if target == "/flash" && (query.is_empty() || query.starts_with("path=")) {
        // The UI stores its current directory in history; /flash stays the
        // recovery endpoint after directory navigation and browser reload.
        argument = "flash=1";
        (Route::Index, "/")
    } else if matches!(target, "/" | "/index.html")
        && (matches!(query, "" | "flash=1") || query.starts_with("path="))
    {
        // Directory history belongs to the browser; a reload still serves the UI.
        argument = if query == "flash=1" { query } else { "" };
        (Route::Index, "/")
    } else if target == "/api/photoframe" && query.is_empty() {
        (Route::PhotoStatus, "/")
    } else if target == "/api/photoframe/stop" && query.is_empty() {
        (Route::PhotoStop, "/")
    } else if target == "/api/photoframe/start" {
        let (path, options) = query
            .strip_prefix("path=")
            .ok_or(400u16)?
            .split_once("&options=")
            .ok_or(400u16)?;
        argument = options;
        (Route::PhotoStart, path)
    } else if target == "/api/list" {
        (Route::List, query.strip_prefix("path=").ok_or(400u16)?)
    } else if target == "/api/rename" {
        let (path, name) = query
            .strip_prefix("path=")
            .ok_or(400u16)?
            .split_once("&name=")
            .ok_or(400u16)?;
        argument = name;
        (Route::Rename, path)
    } else if target == "/api/net-test" {
        argument = query.strip_prefix("bytes=").ok_or(400u16)?;
        (Route::NetTest, "/")
    } else if target == "/api/clipboard" && query.is_empty() {
        (Route::Clipboard, "/")
    } else if target == "/api/state" && query.is_empty() {
        (Route::State, "/")
    } else if target == "/api/resources/release" && query.is_empty() {
        (Route::Release, "/")
    } else if target == "/api/brightness" {
        argument = query.strip_prefix("value=").ok_or(400u16)?;
        (Route::Brightness, "/")
    } else if let Some(path) = target.strip_prefix("/fs/") {
        if !query.is_empty() && query != "download=1" {
            return Err(400);
        }
        download = query == "download=1";
        (Route::File, path)
    } else if target.starts_with('/')
        && !target.starts_with("/api/")
        && !target.starts_with("/fs/")
        && !matches!(target, "/" | "/index.html" | "/flash" | "/api" | "/fs")
    {
        (Route::Static, target)
    } else {
        return Err(404);
    };
    let allowed = match route {
        Route::Index | Route::List | Route::State | Route::Static => {
            matches!(method, Method::Get | Method::Head)
        }
        Route::File => {
            matches!(
                method,
                Method::Get | Method::Head | Method::Put | Method::Delete | Method::Mkcol
            ) || cfg!(feature = "diagnostics") && method == Method::Post
        }
        Route::PhotoStatus => method == Method::Get,
        Route::PhotoStart
        | Route::PhotoStop
        | Route::Rename
        | Route::Brightness
        | Route::Release => method == Method::Post,
        Route::NetTest | Route::Clipboard => {
            matches!(method, Method::Get | Method::Head | Method::Post)
        }
    };
    if !allowed {
        return Err(405);
    }
    if route == Route::File && method == Method::Post && length.is_none() {
        return Err(411);
    }
    if route == Route::File && method == Method::Post && length.unwrap_or(0) > 512 {
        return Err(413);
    }

    if length.unwrap_or(0) != 0
        && !(matches!(route, Route::Clipboard | Route::NetTest) && method == Method::Post
            || route == Route::File
                && (method == Method::Put
                    || cfg!(feature = "diagnostics") && method == Method::Post))
    {
        return Err(400);
    }
    if matches!(route, Route::Clipboard | Route::NetTest)
        && method == Method::Post
        && length.is_none()
    {
        return Err(411);
    }
    let mut request = Request {
        method,
        path: [0; crate::storage::volume::PATH_BYTES],
        path_len: 0,
        length: length.unwrap_or(0),
        expect,
        http11: version == "HTTP/1.1",
        route,
        argument: [0; crate::storage::volume::PATH_BYTES],
        argument_len: 0,
        download,
    };
    if route == Route::Static && !path.starts_with("/www/") && path != "/www" {
        request.path[..4].copy_from_slice(b"/www");
        request.path_len = 4;
    }
    if route == Route::File {
        request.path[0] = b'/';
        request.path_len = 1;
    }
    request.path_len += decode(path, &mut request.path[request.path_len..])?;
    if route == Route::Static && (request.path() == "/www" || request.path().ends_with('/')) {
        let suffix = if request.path() == "/www" {
            b"/index.html".as_slice()
        } else {
            b"index.html".as_slice()
        };
        let end = request.path_len + suffix.len();
        if end > request.path.len() {
            return Err(414);
        }
        request.path[request.path_len..end].copy_from_slice(suffix);
        request.path_len = end;
    }
    request.argument_len = decode(argument, &mut request.argument)?;
    if !request.path().starts_with('/') || request.path().split('/').any(|s| s == ".." || s == ".")
    {
        return Err(400);
    }
    if route == Route::File && (request.path_len <= 1 || request.path().ends_with('/')) {
        return Err(400);
    }
    Ok(request)
}
fn decode(text: &str, out: &mut [u8]) -> Result<usize, u16> {
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut n = 0;
    while i < bytes.len() {
        let b = if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(400);
            }
            let b = hex(bytes[i + 1]).ok_or(400u16)? * 16 + hex(bytes[i + 2]).ok_or(400u16)?;
            i += 3;
            b
        } else {
            let b = bytes[i];
            i += 1;
            if b == b'#' || b == b'&' {
                return Err(400);
            }
            b
        };
        if b < 32 || b == 127 || b == b'\\' || b == b'?' {
            return Err(400);
        }
        if n == out.len() {
            return Err(414);
        }
        out[n] = b;
        n += 1;
    }
    let decoded = core::str::from_utf8(&out[..n]).map_err(|_| 400u16)?;
    if decoded.chars().any(char::is_control) {
        return Err(400);
    }
    Ok(n)
}
fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
pub fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        417 => "Expectation Failed",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        507 => "Insufficient Storage",
        501 => "Not Implemented",
        _ => "Internal Server Error",
    }
}
/// Fixed formatting workspace; overflow is an error, never silent truncation.
pub struct Text<const N: usize> {
    pub bytes: [u8; N],
    pub len: usize,
}
impl<const N: usize> Default for Text<N> {
    fn default() -> Self {
        Self::new()
    }
}
impl<const N: usize> Text<N> {
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }
    pub fn data(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}
impl<const N: usize> fmt::Write for Text<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.len + s.len() > N {
            return Err(fmt::Error);
        }
        self.bytes[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
        self.len += s.len();
        Ok(())
    }
}
pub fn json_string(out: &mut impl fmt::Write, s: &str) -> fmt::Result {
    out.write_char('"')?;
    for c in s.chars() {
        match c {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            c if c.is_control() => write!(out, "\\u{:04x}", c as u32)?,
            c => out.write_char(c)?,
        }
    }
    out.write_char('"')
}
#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    #[test]
    fn static_assets_are_read_only_and_confined_to_www() {
        for (target, path) in [
            ("/photo-tools.js?v=2", "/www/photo-tools.js"),
            ("/www/vendor/app.mjs", "/www/vendor/app.mjs"),
            ("/www", "/www/index.html"),
            ("/assets/", "/www/assets/index.html"),
        ] {
            for method in ["GET", "HEAD"] {
                let raw = std::format!("{method} {target} HTTP/1.1\r\nHost: p4\r\n\r\n");
                let r = parse(raw.as_bytes()).unwrap();
                assert_eq!(r.route, Route::Static);
                assert_eq!(r.path(), path);
            }
        }
        for target in ["/www/../private", "/www/%2e%2e/private", "/../private"] {
            assert!(parse(std::format!("GET {target} HTTP/1.1\r\n\r\n").as_bytes()).is_err());
        }
        assert!(parse(b"PUT /www/app.js HTTP/1.1\r\nContent-Length: 0\r\n\r\n").is_err());
        assert!(parse(b"GET /api/unknown HTTP/1.1\r\n\r\n").is_err());
    }
    #[test]
    fn folder_history_reload_serves_ui_without_becoming_a_file_request() {
        for bytes in [
            b"GET /?path=%2FPHOTOS%2F100D3100 HTTP/1.1\r\nHost: p4\r\n\r\n".as_slice(),
            b"GET /index.html?path=%2FPHOTOS HTTP/1.1\r\nHost: p4\r\n\r\n".as_slice(),
        ] {
            let r = parse(bytes).unwrap();
            assert_eq!(r.route, Route::Index);
            assert_eq!(r.path(), "/");
            assert_eq!(r.argument(), "");
        }
    }
    #[test]
    fn photoframe_start_is_explicit_post_with_a_decoded_folder() {
        let r=parse(b"POST /api/photoframe/start?path=%2FPHOTOS%2F%D0%A2%D0%B5%D1%81%D1%82&options=10,0,0,12 HTTP/1.1\r\nHost: p4\r\n\r\n").unwrap();
        assert_eq!(r.route, Route::PhotoStart);
        assert_eq!(r.path(), "/PHOTOS/Тест");
        assert_eq!(r.argument(), "10,0,0,12");
        assert!(parse(
            b"GET /api/photoframe/start?path=%2F&options=10,0,0,0 HTTP/1.1\r\nHost: p4\r\n\r\n"
        )
        .is_err());
        assert!(parse(
            b"POST /api/photoframe/start?path=%2F..&options=10,0,0,0 HTTP/1.1\r\nHost: p4\r\n\r\n"
        )
        .is_err());
        assert!(parse(
            b"POST /api/photoframe/stop HTTP/1.1\r\nHost: p4\r\nContent-Length: 1\r\n\r\n"
        )
        .is_err());
    }
    #[test]
    fn resource_release_is_explicit_empty_post() {
        let r =
            parse(b"POST /api/resources/release HTTP/1.1\r\nHost: p4\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        assert_eq!(r.route, Route::Release);
        assert_eq!(
            parse(b"GET /api/resources/release HTTP/1.1\r\nHost: p4\r\n\r\n").unwrap_err(),
            405
        );
        assert_eq!(
            parse(b"POST /api/resources/release HTTP/1.1\r\nHost: p4\r\nContent-Length: 1\r\n\r\n")
                .unwrap_err(),
            400
        );
    }
    #[test]
    fn explicit_flash_page_only_accepts_get_head_and_exact_flag() {
        extern crate std;
        for target in [
            "/?flash=1",
            "/index.html?flash=1",
            "/flash",
            "/flash?path=%2Fwww",
        ] {
            for method in ["GET", "HEAD"] {
                let text = std::format!("{method} {target} HTTP/1.1\r\nHost: p4\r\n\r\n");
                let r = parse(text.as_bytes()).unwrap();
                assert_eq!(r.route, Route::Index);
                assert_eq!(r.argument(), "flash=1");
            }
        }
        assert!(parse(b"GET /?flash=0 HTTP/1.1\r\nHost: p4\r\n\r\n").is_err());
        assert!(parse(b"GET /?flash=1&x=1 HTTP/1.1\r\nHost: p4\r\n\r\n").is_err());
        assert_eq!(
            parse(b"POST /?flash=1 HTTP/1.1\r\nHost: p4\r\nContent-Length: 0\r\n\r\n").unwrap_err(),
            405
        );
    }
    #[test]
    fn diagnostic_append_requires_bounded_body() {
        let result = parse(b"POST /fs/LOG.TXT HTTP/1.1\r\nHost: p4\r\nContent-Length: 32\r\n\r\n");
        assert_eq!(result.is_ok(), cfg!(feature = "diagnostics"));
        let missing = parse(b"POST /fs/LOG.TXT HTTP/1.1\r\nHost: p4\r\n\r\n");
        assert_eq!(
            missing.err(),
            Some(if cfg!(feature = "diagnostics") {
                411
            } else {
                405
            })
        );
        let large = parse(b"POST /fs/LOG.TXT HTTP/1.1\r\nHost: p4\r\nContent-Length: 513\r\n\r\n");
        assert_eq!(
            large.err(),
            Some(if cfg!(feature = "diagnostics") {
                413
            } else {
                405
            })
        );
    }

    #[test]
    fn unicode_paths_and_json_are_utf8_not_escaped_bytes() {
        let r = parse(
            b"GET /fs/%D0%A4%D0%B0%D0%B9%D0%BB%20%F0%9F%8E%B5.txt HTTP/1.1\r\nHost: p4\r\n\r\n",
        )
        .unwrap();
        assert_eq!(r.path(), "/Файл 🎵.txt");
        for path in ["%ff", "%d0", "%c2%80", "%00"] {
            let mut raw = Text::<256>::new();
            use core::fmt::Write;
            write!(raw, "GET /fs/{path} HTTP/1.1\r\nHost: p4\r\n\r\n").unwrap();
            assert_eq!(parse(raw.data()), Err(400));
        }
        let mut out = Text::<128>::new();
        json_string(&mut out, "Файл 🎵\n\"").unwrap();
        assert_eq!(
            core::str::from_utf8(out.data()).unwrap(),
            "\"Файл 🎵\\u000a\\\"\""
        );
    }
    #[test]
    fn framing_paths_and_limits() {
        let r=parse(b"PUT /fs/DIR%2FNEW.BIN HTTP/1.1\r\nHost: p4\r\nContent-Length: 4294967295\r\nExpect: 100-continue\r\n\r\n").unwrap();
        assert_eq!(r.path(), "/DIR/NEW.BIN");
        assert_eq!(r.length, u32::MAX);
        assert!(r.expect);
        for (line, code) in [
            ("PUT /fs/X HTTP/1.1\r\nHost: p4\r\n", 411),
            ("GET / HTTP/1.1\r\n", 400),
            ("GET /fs/%2e%2e/X HTTP/1.0\r\n", 400),
            ("GET /fs/%00 HTTP/1.0\r\n", 400),
            ("GET / HTTP/1.0\r\nTransfer-Encoding: chunked\r\n", 501),
            (
                "PUT /fs/X HTTP/1.0\r\nContent-Length: 1\r\nContent-Length: 1\r\n",
                400,
            ),
            ("PUT /fs/X HTTP/1.0\r\nContent-Length: 4294967296\r\n", 413),
        ] {
            let mut t = Text::<256>::new();
            use core::fmt::Write;
            write!(t, "{line}\r\n").unwrap();
            assert_eq!(parse(t.data()).unwrap_err(), code);
        }
    }
    #[test]
    fn split_header_and_json_escaping() {
        let r = parse(b"GET /fs/A%23B.TXT HTTP/1.0\r\n\r\n").unwrap();
        assert_eq!(r.path(), "/A#B.TXT");
        assert_eq!(header_end(b"GET / HTTP/1.0\r\n\r"), None);
        assert_eq!(header_end(b"GET / HTTP/1.0\r\n\r\nBODY"), Some(18));
        let mut t = Text::<64>::new();
        json_string(&mut t, "a\"\\\n").unwrap();
        assert_eq!(t.data(), b"\"a\\\"\\\\\\u000a\"");
    }
}
