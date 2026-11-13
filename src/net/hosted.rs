//! Bounded ESP-Hosted V1 framing, boot TLVs and protobuf wire primitives.
//! Protocol reference: Espressif esp-hosted-mcu (Apache-2.0). No runtime/allocator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Boot {
    pub chip: u8,
    pub capabilities: u8,
    pub raw_tp: u8,
    pub extended: u32,
    pub version: u32,
    pub rpc: u8,
    pub sdio_mode: u8,
    pub buffer_config: [u8; 5],
}
pub fn frame(bytes: &[u8]) -> Result<(u8, &[u8], usize), &'static str> {
    if bytes.len() < 12 {
        return Err("short hosted header");
    }
    let len = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
    let offset = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    if !(12..=19).contains(&offset) || len == 0 || offset + len > bytes.len() {
        return Err("hosted bounds");
    }
    if bytes[1] & 1 != 0 {
        return Err("fragmented hosted frame unsupported");
    }
    let checksum = u16::from_le_bytes([bytes[6], bytes[7]]);
    if checksum != 0 {
        let sum = bytes[..offset + len]
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 6 && *i != 7)
            .fold(0u16, |s, (_, b)| s.wrapping_add(u16::from(*b)));
        if sum != checksum {
            return Err("hosted checksum");
        }
    }
    Ok((bytes[0] & 15, &bytes[offset..offset + len], offset + len))
}
/// Advance a streamed SDIO aggregate. Peers can align adjacent frames to 4B.
/// A malformed remainder is discarded so polling cannot repeat it forever.
pub fn stream_frame<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
) -> Result<(u8, &'a [u8]), &'static str> {
    if *cursor >= bytes.len() {
        return Err("aggregate exhausted");
    }
    let aligned = cursor.next_multiple_of(4);
    let parsed = frame(&bytes[*cursor..]).or_else(|error| {
        if aligned > *cursor
            && aligned < bytes.len()
            && bytes[*cursor..aligned].iter().all(|b| *b == 0)
        {
            *cursor = aligned;
            frame(&bytes[*cursor..])
        } else {
            Err(error)
        }
    });
    match parsed {
        Ok((interface, payload, used)) => {
            *cursor += used;
            Ok((interface, payload))
        }
        Err(error) => {
            *cursor = bytes.len();
            Err(error)
        }
    }
}
pub fn boot(payload: &[u8]) -> Result<Boot, &'static str> {
    if payload.len() < 2 || payload[0] != 0x22 || payload[1] as usize != payload.len() - 2 {
        return Err("boot event bounds/type");
    }
    let mut out = Boot {
        chip: 0xff,
        capabilities: 0,
        raw_tp: 0,
        extended: 0,
        version: 0,
        rpc: 0,
        sdio_mode: 0xff,
        buffer_config: [0; 5],
    };
    let mut bytes = &payload[2..];
    while !bytes.is_empty() {
        if bytes.len() < 2 || bytes[1] as usize > bytes.len() - 2 {
            return Err("boot TLV bounds");
        }
        let (tag, len) = (bytes[0], bytes[1] as usize);
        let v = &bytes[2..2 + len];
        match (tag, len) {
            (0x12, 1) => out.chip = v[0],
            (0x11, 1) => out.capabilities = v[0],
            (0x13, 1) => out.raw_tp = v[0],
            (0x16, 4) => out.extended = u32::from_le_bytes(v.try_into().unwrap()),
            (0x17, 4) => out.version = u32::from_le_bytes(v.try_into().unwrap()),
            (0x1a, 1) => out.rpc = v[0],
            (0x18, 1) => out.sdio_mode = v[0],
            (0x1b, 5) => out.buffer_config.copy_from_slice(v),
            _ => {}
        }
        bytes = &bytes[2 + len..];
    }
    if out.chip != 0x0d || out.capabilities & 1 == 0 || out.rpc != 2 {
        return Err("unsupported C6 capabilities/RPC");
    }
    Ok(out)
}
pub fn encode_frame(
    out: &mut [u8],
    interface: u8,
    sequence: u16,
    payload: &[u8],
    checksum: bool,
) -> Result<usize, &'static str> {
    let len = 12 + payload.len();
    if len > out.len() || payload.len() > u16::MAX as usize {
        return Err("TX capacity");
    }
    out[..len].fill(0);
    out[0] = interface;
    out[2..4].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    out[4] = 12;
    out[8..10].copy_from_slice(&sequence.to_le_bytes());
    out[12..len].copy_from_slice(payload);
    if checksum {
        let sum = out[..len]
            .iter()
            .fold(0u16, |s, b| s.wrapping_add(u16::from(*b)));
        out[6..8].copy_from_slice(&sum.to_le_bytes());
    }
    Ok(len)
}
pub struct Encoder<'a> {
    pub out: &'a mut [u8],
    pub len: usize,
}
impl<'a> Encoder<'a> {
    pub fn new(out: &'a mut [u8]) -> Self {
        Self { out, len: 0 }
    }
    fn varint(&mut self, mut value: u64) -> Result<(), &'static str> {
        loop {
            if self.len == self.out.len() {
                return Err("protobuf TX capacity");
            }
            self.out[self.len] = (value as u8 & 127) | if value >= 128 { 128 } else { 0 };
            self.len += 1;
            value >>= 7;
            if value == 0 {
                return Ok(());
            }
        }
    }
    pub fn scalar(&mut self, field: u32, value: u64) -> Result<(), &'static str> {
        self.varint(u64::from(field) * 8)?;
        self.varint(value)
    }
    pub fn bytes(&mut self, field: u32, bytes: &[u8]) -> Result<(), &'static str> {
        self.varint(u64::from(field) * 8 + 2)?;
        self.varint(bytes.len() as u64)?;
        if bytes.len() > self.out.len() - self.len {
            return Err("protobuf TX capacity");
        }
        self.out[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        Ok(())
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Value<'a> {
    Scalar(u64),
    Bytes(&'a [u8]),
}
pub fn field<'a>(bytes: &mut &'a [u8]) -> Result<Option<(u32, Value<'a>)>, &'static str> {
    if bytes.is_empty() {
        return Ok(None);
    }
    fn varint(bytes: &mut &[u8]) -> Result<u64, &'static str> {
        let mut out = 0;
        for i in 0..10 {
            let Some((&b, rest)) = bytes.split_first() else {
                return Err("truncated varint");
            };
            *bytes = rest;
            if i == 9 && b > 1 {
                return Err("varint overflow");
            }
            out |= u64::from(b & 127) << (7 * i);
            if b & 128 == 0 {
                return Ok(out);
            }
        }
        Err("varint overflow")
    }
    let key = varint(bytes)?;
    let number = u32::try_from(key >> 3).map_err(|_| "protobuf field overflow")?;
    if number == 0 {
        return Err("protobuf field zero");
    }
    let value = match key & 7 {
        0 => Value::Scalar(varint(bytes)?),
        1 | 5 => {
            let n = if key & 7 == 1 { 8 } else { 4 };
            if bytes.len() < n {
                return Err("truncated fixed");
            }
            let v = &bytes[..n];
            *bytes = &bytes[n..];
            Value::Bytes(v)
        }
        2 => {
            let n = usize::try_from(varint(bytes)?).map_err(|_| "protobuf length overflow")?;
            if n > bytes.len() {
                return Err("truncated bytes");
            }
            let v = &bytes[..n];
            *bytes = &bytes[n..];
            Value::Bytes(v)
        }
        _ => return Err("unsupported protobuf wire type"),
    };
    Ok(Some((number, value)))
}
pub fn serial(payload: &[u8]) -> Result<&[u8], &'static str> {
    if payload.len() < 3 || payload[0] != 1 {
        return Err("serial endpoint tag");
    }
    let n = u16::from_le_bytes([payload[1], payload[2]]) as usize;
    let at = 3 + n;
    if payload.len() < at + 3 || payload[at] != 2 {
        return Err("serial data tag");
    }
    let len = u16::from_le_bytes([payload[at + 1], payload[at + 2]]) as usize;
    if payload.len() != at + 3 + len {
        return Err("serial data bounds");
    }
    Ok(&payload[at + 3..])
}
#[cfg(test)]
mod tests {
    use super::*;
    const INIT: [u8; 54] = [
        5, 0, 42, 0, 12, 0, 0, 0, 0, 0, 0, 51, 34, 40, 18, 1, 13, 17, 1, 1, 19, 1, 1, 20, 1, 20,
        21, 1, 20, 23, 4, 9, 0, 3, 0, 22, 4, 16, 0, 0, 0, 26, 1, 2, 24, 1, 1, 27, 5, 0, 0, 31, 0,
        31,
    ];
    #[test]
    fn stream_handles_packed_and_aligned_frames_and_discards_bad_tail() {
        for padding in [false, true] {
            let mut bytes = [0u8; 64];
            let first = encode_frame(&mut bytes, 1, 0, b"ab", true).unwrap();
            let next = if padding {
                first.next_multiple_of(4)
            } else {
                first
            };
            let second = encode_frame(&mut bytes[next..], 1, 1, b"cd", true).unwrap();
            let data = &bytes[..next + second];
            let mut cursor = 0;
            assert_eq!(stream_frame(data, &mut cursor).unwrap(), (1, &b"ab"[..]));
            assert_eq!(stream_frame(data, &mut cursor).unwrap(), (1, &b"cd"[..]));
            assert_eq!(cursor, data.len());
        }
        let mut cursor = 0;
        assert!(stream_frame(&[255; 20], &mut cursor).is_err());
        assert_eq!(cursor, 20);
    }
    #[test]
    fn captured_boot_and_truncations() {
        let (interface, payload, end) = frame(&INIT).unwrap();
        assert_eq!((interface, end), (5, 54));
        let info = boot(payload).unwrap();
        assert_eq!((info.chip, info.version, info.rpc), (13, 0x30009, 2));
        assert_eq!(info.raw_tp, 1);
        for n in 0..INIT.len() {
            assert!(frame(&INIT[..n]).is_err());
        }
        for n in 0..payload.len() {
            assert!(boot(&payload[..n]).is_err());
        }
        let mut buf = [0; 128];
        let n = encode_frame(&mut buf, 5, 17, payload, true).unwrap();
        assert!(frame(&buf[..n]).is_ok());
        buf[15] ^= 1;
        assert!(frame(&buf[..n]).is_err());
    }
    #[test]
    fn protobuf_bounds_and_unknown_fields() {
        let mut out = [0; 64];
        let mut e = Encoder::new(&mut out);
        e.scalar(1, u64::MAX).unwrap();
        e.bytes(500, b"abc").unwrap();
        let n = e.len;
        let mut b = &out[..n];
        assert_eq!(field(&mut b).unwrap(), Some((1, Value::Scalar(u64::MAX))));
        assert_eq!(field(&mut b).unwrap(), Some((500, Value::Bytes(b"abc"))));
        assert!(field(&mut b).unwrap().is_none());
        for bad in [
            &[8, 128][..],
            &[0][..],
            &[10, 255, 127][..],
            &[8, 255, 255, 255, 255, 255, 255, 255, 255, 255, 2][..],
        ] {
            assert!(field(&mut &bad[..]).is_err());
        }
    }
}
