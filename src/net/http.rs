//! Small strict HTTP/1 parser. One request per connection; PUT has a known length.
//! Transfer-Encoding is rejected, never guessed from connection closure.
use core::fmt;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Put,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub path: [u8; 96],
    pub path_len: usize,
    pub length: u32,
    pub expect: bool,
    pub http11: bool,
    pub route: Route,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Index,
    List,
    File,
}
impl Request {
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
    if method != Method::Put && length.unwrap_or(0) != 0 {
        return Err(400);
    }
    let (route, path) = if target == "/" || target.starts_with("/?") || target == "/index.html" {
        (Route::Index, "/")
    } else if let Some(path) = target.strip_prefix("/api/list?path=") {
        (Route::List, path)
    } else if let Some(path) = target.strip_prefix("/fs/") {
        (Route::File, path)
    } else {
        return Err(404);
    };
    if method == Method::Put && route != Route::File {
        return Err(405);
    }
    let mut request = Request {
        method,
        path: [0; 96],
        path_len: 0,
        length: length.unwrap_or(0),
        expect,
        http11: version == "HTTP/1.1",
        route,
    };
    if route == Route::File {
        request.path[0] = b'/';
        request.path_len = 1;
    }
    if path.contains('#') {
        return Err(400);
    }
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(400);
            }
            let hi = hex(bytes[i + 1]).ok_or(400u16)?;
            let lo = hex(bytes[i + 2]).ok_or(400u16)?;
            i += 3;
            hi * 16 + lo
        } else {
            let b = bytes[i];
            i += 1;
            b
        };
        if !b.is_ascii_graphic() || b == b'\\' || b == b'?' {
            return Err(400);
        }
        if request.path_len == request.path.len() {
            return Err(414);
        }
        request.path[request.path_len] = b;
        request.path_len += 1;
    }
    if !request.path().starts_with('/') || request.path().split('/').any(|s| s == ".." || s == ".")
    {
        return Err(400);
    }
    if route == Route::File && (request.path_len <= 1 || request.path().ends_with('/')) {
        return Err(400);
    }
    Ok(request)
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
    for b in s.bytes() {
        match b {
            b'"' => out.write_str("\\\"")?,
            b'\\' => out.write_str("\\\\")?,
            0..=31 | 127..=255 => write!(out, "\\u{b:04x}")?,
            _ => out.write_char(b as char)?,
        }
    }
    out.write_char('"')
}
#[cfg(test)]
mod tests {
    use super::*;
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
