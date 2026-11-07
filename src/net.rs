//! Minimal bounded Ethernet/IPv4 packet logic for bring-up, no sockets or TCP.
//! DHCP client (one lease), ARP reply and ICMP echo. No allocations or fragments.
pub const MAC: [u8; 6] = [0x02, 0xf1, 0xb2, 0xd3, 0x81, 0x00];
pub const FRAME: usize = 1536;

pub fn checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    for pair in bytes.chunks(2) {
        sum += u32::from(pair[0]) << 8 | u32::from(*pair.get(1).unwrap_or(&0));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}
fn word(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}
fn put(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_be_bytes());
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease {
    pub address: [u8; 4],
    pub server: [u8; 4],
    pub mask: [u8; 4],
    pub router: [u8; 4],
    pub seconds: u32,
}

/// DHCPDISCOVER or DHCPREQUEST for the selected offer; UDP checksum omitted
/// as permitted for IPv4. DHCP flag requests a broadcast server response.
pub fn dhcp(out: &mut [u8; FRAME], xid: u32, offer: Option<Lease>) -> usize {
    out.fill(0);
    out[..6].fill(255);
    out[6..12].copy_from_slice(&MAC);
    put(out, 12, 0x0800);
    out[14] = 0x45;
    out[22] = 64;
    out[23] = 17;
    out[30..34].fill(255);
    put(out, 34, 68);
    put(out, 36, 67);
    let boot = &mut out[42..];
    boot[..3].copy_from_slice(&[1, 1, 6]);
    boot[4..8].copy_from_slice(&xid.to_be_bytes());
    put(boot, 10, 0x8000);
    boot[28..34].copy_from_slice(&MAC);
    boot[236..240].copy_from_slice(&[99, 130, 83, 99]);
    let mut at = 240;
    boot[at..at + 3].copy_from_slice(&[53, 1, if offer.is_some() { 3 } else { 1 }]);
    at += 3;
    boot[at..at + 3].copy_from_slice(&[61, 7, 1]);
    at += 3;
    boot[at..at + 6].copy_from_slice(&MAC);
    at += 6;
    if let Some(offer) = offer {
        for (code, value) in [(50, offer.address), (54, offer.server)] {
            boot[at..at + 2].copy_from_slice(&[code, 4]);
            at += 2;
            boot[at..at + 4].copy_from_slice(&value);
            at += 4;
        }
    }
    boot[at..at + 5].copy_from_slice(&[55, 3, 1, 3, 6]);
    at += 5;
    boot[at..at + 9].copy_from_slice(&[12, 7, b'p', b'4', b'-', b'r', b'u', b's', b't']);
    at += 9;
    boot[at] = 255;
    at += 1;
    // RFC 2131 clients must support at least the 300-byte BOOTP message.
    at = at.max(300);
    put(out, 38, (8 + at) as u16);
    put(out, 16, (28 + at) as u16);
    let sum = checksum(&out[14..34]);
    put(out, 24, sum);
    42 + at
}

fn ipv4(frame: &[u8]) -> Option<(&[u8], usize)> {
    if frame.len() < 34 || word(frame, 12) != 0x0800 || frame[14] >> 4 != 4 {
        return None;
    }
    let ihl = usize::from(frame[14] & 15) * 4;
    let total = usize::from(word(frame, 16));
    if ihl < 20 || total < ihl || 14 + total > frame.len() || word(frame, 20) & 0x3fff != 0 {
        return None;
    }
    let ip = &frame[14..14 + total];
    if checksum(&ip[..ihl]) != 0 {
        return None;
    }
    Some((ip, ihl))
}

pub fn dhcp_reply(frame: &[u8], xid: u32) -> Option<(u8, Lease)> {
    let (ip, ihl) = ipv4(frame)?;
    if ip[9] != 17 || ip.len() < ihl + 8 {
        return None;
    }
    let udp = &ip[ihl..];
    let size = usize::from(word(udp, 4));
    if word(udp, 0) != 67 || word(udp, 2) != 68 || size < 248 || size > udp.len() {
        return None;
    }
    // Validate nonzero UDP checksum, including the IPv4 pseudo-header.
    if word(udp, 6) != 0 {
        let mut pseudo = [0u8; 12];
        pseudo[..8].copy_from_slice(&ip[12..20]);
        pseudo[9] = 17;
        put(&mut pseudo, 10, size as u16);
        let mut sum = u32::from(!checksum(&pseudo)) + u32::from(!checksum(&udp[..size]));
        while sum >> 16 != 0 {
            sum = (sum & 65535) + (sum >> 16);
        }
        if sum != 65535 {
            return None;
        }
    }
    let boot = &udp[8..size];
    if boot[..3] != [2, 1, 6]
        || boot[4..8] != xid.to_be_bytes()
        || boot[28..34] != MAC
        || boot[236..240] != [99, 130, 83, 99]
    {
        return None;
    }
    let mut lease = Lease {
        address: boot[16..20].try_into().ok()?,
        server: [0; 4],
        mask: [0; 4],
        router: [0; 4],
        seconds: 0,
    };
    let mut kind = 0;
    let mut options = &boot[240..];
    while let Some((&code, tail)) = options.split_first() {
        options = tail;
        if code == 255 {
            break;
        }
        if code == 0 {
            continue;
        }
        let (&len, tail) = options.split_first()?;
        let value = tail.get(..usize::from(len))?;
        options = &tail[usize::from(len)..];
        match (code, value.len()) {
            (53, 1) => kind = value[0],
            (54, 4) => lease.server.copy_from_slice(value),
            (1, 4) => lease.mask.copy_from_slice(value),
            (3, 4..) => lease.router.copy_from_slice(&value[..4]),
            (51, 4) => lease.seconds = u32::from_be_bytes(value.try_into().ok()?),
            _ => {}
        }
    }
    if !matches!(kind, 2 | 5 | 6) {
        return None;
    }
    Some((kind, lease))
}

/// Returns a reply frame length for ARP or ICMP echo addressed to this lease.
pub fn reply(frame: &[u8], ip: [u8; 4], out: &mut [u8; FRAME]) -> Option<usize> {
    if frame.len() < 14 || ip == [0; 4] {
        return None;
    }
    if word(frame, 12) == 0x0806 {
        if frame.len() < 42 || frame[14..22] != [0, 1, 8, 0, 6, 4, 0, 1] || frame[38..42] != ip {
            return None;
        }
        out[..42].copy_from_slice(&frame[..42]);
        out[..6].copy_from_slice(&frame[22..28]);
        out[6..12].copy_from_slice(&MAC);
        put(out, 20, 2);
        out[22..28].copy_from_slice(&MAC);
        out[28..32].copy_from_slice(&ip);
        out[32..38].copy_from_slice(&frame[22..28]);
        out[38..42].copy_from_slice(&frame[28..32]);
        return Some(42);
    }
    let (packet, ihl) = ipv4(frame)?;
    if packet[9] != 1 || packet[16..20] != ip || packet.len() < ihl + 8 {
        return None;
    }
    let icmp = &packet[ihl..];
    if icmp[0] != 8 || icmp[1] != 0 || checksum(icmp) != 0 {
        return None;
    }
    let len = 14 + packet.len();
    if len > FRAME {
        return None;
    }
    out[..len].copy_from_slice(&frame[..len]);
    out[..6].copy_from_slice(&frame[6..12]);
    out[6..12].copy_from_slice(&MAC);
    out[26..30].copy_from_slice(&ip);
    out[30..34].copy_from_slice(&packet[12..16]);
    out[22] = 64;
    put(out, 24, 0);
    let sum = checksum(&out[14..14 + ihl]);
    put(out, 24, sum);
    out[14 + ihl] = 0;
    put(out, 14 + ihl + 2, 0);
    let sum = checksum(&out[14 + ihl..len]);
    put(out, 14 + ihl + 2, sum);
    Some(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    const IP: [u8; 4] = [192, 168, 1, 167];
    const SERVER: [u8; 4] = [192, 168, 1, 1];
    const XID: u32 = 0x12345678;
    fn incoming_dhcp(out: &mut [u8; FRAME]) -> usize {
        let n = dhcp(
            out,
            XID,
            Some(Lease {
                address: IP,
                server: SERVER,
                mask: [255, 255, 255, 0],
                router: SERVER,
                seconds: 3600,
            }),
        );
        put(out, 34, 67);
        put(out, 36, 68);
        out[26..30].copy_from_slice(&SERVER);
        out[42] = 2;
        out[58..62].copy_from_slice(&IP);
        out[284] = 5;
        // Add lease lifetime before END (the rest is zero padding).
        let end = out[282..n].iter().position(|&v| v == 255).unwrap() + 282;
        out[end..end + 7].copy_from_slice(&[51, 4, 0, 0, 14, 16, 255]);
        put(out, 24, 0);
        let c = checksum(&out[14..34]);
        put(out, 24, c);
        n
    }
    #[test]
    fn internet_checksum_known_vector_and_odd_length() {
        assert_eq!(checksum(&[0, 1, 0xf2, 3, 0xf4, 0xf5, 0xf6, 0xf7]), 0x220d);
        assert_eq!(checksum(&[1]), 0xfeff);
    }
    #[test]
    fn dhcp_valid_reply_rejects_truncation_wrong_client_and_bad_options() {
        let mut bytes = [0; FRAME];
        let n = incoming_dhcp(&mut bytes);
        let (kind, lease) = dhcp_reply(&bytes[..n], XID).unwrap();
        assert_eq!(kind, 5);
        assert_eq!(lease.address, IP);
        assert_eq!(lease.server, SERVER);
        assert_eq!(lease.seconds, 3600);
        for len in 0..n {
            assert!(dhcp_reply(&bytes[..len], XID).is_none(), "len={len}");
        }
        assert!(dhcp_reply(&bytes[..n], XID + 1).is_none());
        bytes[70] ^= 1;
        assert!(dhcp_reply(&bytes[..n], XID).is_none());
        bytes[70] ^= 1;
        bytes[283] = 255;
        assert!(dhcp_reply(&bytes[..n], XID).is_none());
    }
    #[test]
    fn dhcp_nonzero_udp_checksum_is_validated() {
        let mut bytes = [0; FRAME];
        let n = incoming_dhcp(&mut bytes);
        let mut pseudo = [0; 12];
        pseudo[..8].copy_from_slice(&bytes[26..34]);
        pseudo[9] = 17;
        put(&mut pseudo, 10, (n - 34) as u16);
        let mut sum = u32::from(!checksum(&pseudo)) + u32::from(!checksum(&bytes[34..n]));
        while sum >> 16 != 0 {
            sum = (sum & 65535) + (sum >> 16);
        }
        put(&mut bytes, 40, !(sum as u16));
        assert!(dhcp_reply(&bytes[..n], XID).is_some());
        bytes[n - 1] ^= 1;
        assert!(dhcp_reply(&bytes[..n], XID).is_none());
    }
    #[test]
    fn arp_reply_targets_sender_and_ignores_other_addresses() {
        let mut request = [0; 42];
        request[..6].fill(255);
        request[6..12].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
        put(&mut request, 12, 0x0806);
        request[14..22].copy_from_slice(&[0, 1, 8, 0, 6, 4, 0, 1]);
        request[22..28].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
        request[28..32].copy_from_slice(&SERVER);
        request[38..42].copy_from_slice(&IP);
        let mut out = [0; FRAME];
        assert_eq!(reply(&request, IP, &mut out), Some(42));
        assert_eq!(&out[..6], &request[22..28]);
        assert_eq!(&out[22..28], &MAC);
        assert_eq!(word(&out, 20), 2);
        assert_eq!(&out[28..32], &IP);
        assert_eq!(&out[38..42], &SERVER);
        assert!(reply(&request, SERVER, &mut out).is_none());
        for n in 0..42 {
            assert!(reply(&request[..n], IP, &mut out).is_none());
        }
    }
    #[test]
    fn icmp_echo_preserves_payload_checksums_and_rejects_fragments() {
        let mut request = [0; FRAME];
        let n = 14 + 24 + 8 + 37;
        request[..6].copy_from_slice(&MAC);
        request[6..12].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
        put(&mut request, 12, 0x0800);
        request[14] = 0x46;
        put(&mut request, 16, (n - 14) as u16);
        request[22] = 55;
        request[23] = 1;
        request[26..30].copy_from_slice(&SERVER);
        request[30..34].copy_from_slice(&IP);
        request[38] = 8;
        request[42..n].fill(0xa5);
        let c = checksum(&request[38..n]);
        put(&mut request, 40, c);
        let c = checksum(&request[14..38]);
        put(&mut request, 24, c);
        let mut out = [0; FRAME];
        assert_eq!(reply(&request[..n], IP, &mut out), Some(n));
        assert_eq!(checksum(&out[14..38]), 0);
        assert_eq!(checksum(&out[38..n]), 0);
        assert_eq!(out[38], 0);
        assert_eq!(&out[42..n], &request[42..n]);
        assert_eq!(&out[30..34], &SERVER);
        request[n - 1] ^= 1;
        assert!(reply(&request[..n], IP, &mut out).is_none());
        request[n - 1] ^= 1;
        put(&mut request, 20, 0x2000);
        put(&mut request, 24, 0);
        let c = checksum(&request[14..38]);
        put(&mut request, 24, c);
        assert!(reply(&request[..n], IP, &mut out).is_none());
    }
}
