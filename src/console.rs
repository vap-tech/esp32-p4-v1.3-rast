//! Line-framed console: only ':' at start of line and a complete LF/CRLF frame
//! can dispatch. Noise, echo, invalid bytes, overflow and stale fragments are dropped.
pub const COMMANDS: &str = include_str!("../commands.tsv");
pub const ROUTES: &str = include_str!("../command-routes.tsv");
pub struct Input {
    bytes: [u8; 256],
    len: usize,
    dirty: bool,
    active: bool,
    started: u64,
}
impl Default for Input {
    fn default() -> Self {
        Self {
            bytes: [0; 256],
            len: 0,
            dirty: false,
            active: false,
            started: 0,
        }
    }
}
impl Input {
    pub fn clear(&mut self) {
        for byte in &mut self.bytes {
            unsafe {
                core::ptr::write_volatile(byte, 0);
            }
        }
        self.len = 0;
        self.active = false;
        self.dirty = false;
    }
    pub fn feed(&mut self, byte: u8, now: u64) -> Option<Result<&str, &'static str>> {
        if self.active && now.wrapping_sub(self.started) > 2_000_000 {
            self.dirty = true;
        }
        if byte == b'\n' {
            let active = self.active;
            let dirty = self.dirty;
            self.active = false;
            self.dirty = false;
            let len = self.len;
            self.len = 0;
            if !active {
                return None;
            }
            if dirty {
                return Some(Err("invalid, oversized or timed-out command"));
            }
            let bytes = &self.bytes[..len];
            let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
            return Some(core::str::from_utf8(bytes).map_err(|_| "ASCII required"));
        }
        if !self.active {
            if !self.dirty && byte == b':' {
                self.active = true;
                self.started = now;
                self.len = 0;
            } else {
                self.dirty = true;
            }
        } else if self.len == self.bytes.len() {
            self.dirty = true;
        } else {
            if !byte.is_ascii_graphic() && byte != b' ' && byte != b'\r' {
                self.dirty = true;
            }
            self.bytes[self.len] = byte;
            self.len += 1;
        }
        None
    }
}
pub fn entries(table: &str) -> impl Iterator<Item = (&str, &str)> {
    table
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('\t'))
}
pub fn action(line: &str) -> Option<u8> {
    entries(COMMANDS)
        .find(|(name, _)| *name == line)
        .map(|(_, code)| code.as_bytes()[0])
}
pub fn route<'a, const N: usize>(line: &str, buffer: &'a mut [u8; N]) -> Option<&'a str> {
    for (name, handler) in entries(ROUTES) {
        let args = if line == name {
            ""
        } else if let Some(args) = line.strip_prefix(name).and_then(|s| s.strip_prefix(' ')) {
            args
        } else {
            continue;
        };
        let n = handler.len() + usize::from(!args.is_empty()) + args.len();
        if n > buffer.len() {
            return None;
        }
        buffer[..handler.len()].copy_from_slice(handler.as_bytes());
        if !args.is_empty() {
            buffer[handler.len()] = b' ';
            buffer[handler.len() + 1..n].copy_from_slice(args.as_bytes());
        }
        return core::str::from_utf8(&buffer[..n]).ok();
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_frame_routes_and_clear_erases_input() {
        let mut input = Input::default();
        input.feed(b':', 0);
        for _ in 0..220 {
            input.feed(b'A', 0);
        }
        assert_eq!(input.feed(b'\n', 0).unwrap().unwrap().len(), 220);
        input.clear();
        assert!(input.bytes.iter().all(|b| *b == 0));
        let mut buffer = [0; 256];
        assert_eq!(
            route("wifi connect 4142 3132333435363738 60", &mut buffer),
            Some("c6-connect 4142 3132333435363738 60")
        );
    }
    #[test]
    fn echoed_boot_and_keys_cannot_dispatch() {
        let mut input = Input::default();
        for b in b"MEMORY BOOT CACHE\r\nB?R\nRTC DATA: :display test bist\n" {
            assert!(input.feed(*b, 0).is_none());
        }
        for b in b":help\r" {
            assert!(input.feed(*b, 0).is_none());
        }
        assert_eq!(input.feed(b'\n', 0), Some(Ok("help")));
        assert!(action("B").is_none());
    }
    #[test]
    fn reject_and_recover_frames() {
        let mut input = Input::default();
        input.feed(b':', 0);
        for _ in 0..257 {
            input.feed(b'B', 0);
        }
        assert!(input.feed(b'\n', 0).unwrap().is_err());
        input.feed(b':', 0);
        input.feed(b'h', 2_000_001);
        assert!(input.feed(b'\n', 2_000_002).unwrap().is_err());
        input.feed(b':', 3_000_000);
        input.feed(0xff, 3_000_000);
        assert!(input.feed(b'\n', 3_000_000).unwrap().is_err());
        for b in b":help" {
            input.feed(*b, 4_000_000);
        }
        assert_eq!(input.feed(b'\n', 4_000_000), Some(Ok("help")));
    }
    #[test]
    fn registry_is_unambiguous_and_routes_by_word() {
        for (i, (name, key)) in entries(COMMANDS).enumerate() {
            assert_eq!(key.len(), 1);
            assert!(!name.contains(['\r', '\n', ':']));
            for (other, _) in entries(COMMANDS).skip(i + 1) {
                assert_ne!(name, other);
            }
            assert_eq!(action(name), Some(key.as_bytes()[0]));
        }
        let mut b = [0; 96];
        assert_eq!(
            route("display image HOME.JPG", &mut b),
            Some("image HOME.JPG")
        );
        assert!(route("display images HOME.JPG", &mut b).is_none());
        assert!(route("B", &mut b).is_none());
    }
}

/// Printable action name for diagnostics; internal byte never appears on the wire.
pub fn action_name(code: u8) -> &'static str {
    entries(COMMANDS)
        .find(|(_, value)| value.as_bytes()[0] == code)
        .map(|(name, _)| name)
        .unwrap_or("unknown")
}
