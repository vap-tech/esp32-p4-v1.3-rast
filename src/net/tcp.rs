//! Bounded IPv4 TCP listener for the local HTTP server. One outstanding segment
//! per connection, cumulative ACKs, retransmission and receive backpressure.
//! No IP fragments, out-of-order queue, SACK, window scaling or congestion window
//! growth. Stop-and-wait bounds flight to one MSS; intended for a trusted LAN.
use super::{checksum, ipv4, put, word, FRAME, MAC};
pub const SLOTS: usize = 4;
pub const RX: usize = 2048;
pub const MSS: usize = 1460;
const FIN: u8 = 1;
const SYN: u8 = 2;
const RST: u8 = 4;
const ACK: u8 = 16;
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Closed,
    Syn,
    Open,
    Closing,
    TimeWait,
    Reset,
}
#[derive(Clone, Copy)]
struct Peer {
    mac: [u8; 6],
    ip: [u8; 4],
    port: u16,
}
impl Peer {
    const EMPTY: Self = Self {
        mac: [0; 6],
        ip: [0; 4],
        port: 0,
    };
}
struct Socket {
    state: State,
    peer: Peer,
    recv: u32,
    send: u32,
    pending: usize,
    flags: u8,
    tx: [u8; MSS],
    rx: [u8; RX],
    used: usize,
    ack: bool,
    eof: bool,
    held: bool,
    served: bool,
    sent: bool,
    window: usize,
    mss: usize,
    last: u64,
    retry: u64,
    retries: u8,
}
impl Socket {
    const fn new() -> Self {
        Self {
            state: State::Closed,
            peer: Peer::EMPTY,
            recv: 0,
            send: 0,
            pending: 0,
            flags: 0,
            tx: [0; MSS],
            rx: [0; RX],
            used: 0,
            ack: false,
            eof: false,
            held: false,
            served: false,
            sent: false,
            window: 0,
            mss: 0,
            last: 0,
            retry: 0,
            retries: 0,
        }
    }
    fn reset(&mut self) {
        self.state = State::Closed;
        self.used = 0;
        self.pending = 0;
        self.ack = false;
    }
    fn flight(&self) -> u32 {
        self.pending as u32 + u32::from(self.flags & (SYN | FIN) != 0)
    }
}
#[derive(Clone, Copy)]
struct Tomb {
    peer: Peer,
    recv: u32,
    send: u32,
    until: u64,
}
impl Tomb {
    const EMPTY: Self = Self {
        peer: Peer::EMPTY,
        recv: 0,
        send: 0,
        until: 0,
    };
}
#[derive(Default, Debug)]
pub struct Stats {
    pub accepted: u32,
    pub retransmits: u32,
    pub resets: u32,
    pub timeouts: u32,
}
pub struct Stack {
    sockets: [Socket; SLOTS],
    tombs: [Tomb; 32],
    next_tomb: usize,
    serial: u32,
    pub stats: Stats,
}
impl Default for Stack {
    fn default() -> Self {
        Self::new()
    }
}
impl Stack {
    pub const fn new() -> Self {
        Self {
            sockets: [const { Socket::new() }; SLOTS],
            tombs: [Tomb::EMPTY; 32],
            next_tomb: 0,
            serial: 0,
            stats: Stats {
                accepted: 0,
                retransmits: 0,
                resets: 0,
                timeouts: 0,
            },
        }
    }
    /// Reuse only after all application sessions and EMAC have stopped.
    pub fn clear(&mut self) {
        for s in &mut self.sockets {
            s.reset();
            s.held = false;
        }
        self.tombs.fill(Tomb::EMPTY);
        self.stats = Stats::default();
    }
    pub fn request_pending(&self, i: usize) -> bool {
        let s = &self.sockets[i];
        s.state == State::Open && !s.held && !s.served && s.used != 0
    }
    pub fn ready(&self) -> Option<usize> {
        self.sockets
            .iter()
            .position(|s| s.state == State::Open && !s.held && !s.served && s.used != 0)
    }
    pub fn hold(&mut self, i: usize) {
        self.sockets[i].held = true;
    }
    pub fn release(&mut self, i: usize) {
        let s = &mut self.sockets[i];
        s.held = false;
        s.served = true;
        if s.state == State::Open {
            s.state = State::Closing;
        }
    }
    pub fn alive(&self, i: usize) -> bool {
        matches!(self.sockets[i].state, State::Open | State::Closing)
    }
    pub fn eof(&self, i: usize) -> bool {
        self.sockets[i].eof || !self.alive(i)
    }
    pub fn buffered(&self, i: usize) -> &[u8] {
        &self.sockets[i].rx[..self.sockets[i].used]
    }
    pub fn consume(&mut self, i: usize, n: usize) {
        if n == 0 {
            return;
        }
        let s = &mut self.sockets[i];
        assert!(n <= s.used);
        s.rx.copy_within(n..s.used, 0);
        s.used -= n;
        s.ack = true;
    }
    pub fn read(&mut self, i: usize, out: &mut [u8]) -> usize {
        let n = out.len().min(self.sockets[i].used);
        out[..n].copy_from_slice(&self.sockets[i].rx[..n]);
        self.consume(i, n);
        n
    }
    pub fn writable(&self, i: usize) -> usize {
        let s = &self.sockets[i];
        if s.state == State::Open && s.flight() == 0 {
            s.window.min(s.mss).min(MSS)
        } else {
            0
        }
    }
    pub fn write(&mut self, i: usize, data: &[u8]) -> usize {
        let n = data.len().min(self.writable(i));
        if n == 0 {
            return 0;
        }
        let s = &mut self.sockets[i];
        s.tx[..n].copy_from_slice(&data[..n]);
        s.pending = n;
        s.flags = ACK | 8;
        s.sent = false;
        s.retries = 0;
        n
    }
    pub fn flushed(&self, i: usize) -> bool {
        self.sockets[i].flight() == 0
    }
    pub fn abort(&mut self, i: usize) {
        let s = &mut self.sockets[i];
        if s.state != State::Closed {
            s.send = s.send.wrapping_add(if s.sent { s.flight() } else { 0 });
            s.pending = 0;
            s.flags = 0;
            s.used = 0;
            s.ack = false;
            s.state = State::Reset;
        }
    }
    fn bury(&mut self, i: usize, now: u64) {
        let s = &mut self.sockets[i];
        self.tombs[self.next_tomb] = Tomb {
            peer: s.peer,
            recv: s.recv,
            send: s.send,
            until: now + 120_000_000,
        };
        self.next_tomb = (self.next_tomb + 1) % self.tombs.len();
        s.reset();
    }
    pub fn ingest(
        &mut self,
        frame: &[u8],
        ip: [u8; 4],
        now: u64,
        out: &mut [u8; FRAME],
    ) -> Option<usize> {
        let (p, ihl) = ipv4(frame)?;
        if p[9] != 6 || p[16..20] != ip || p.len() < ihl + 20 {
            return None;
        }
        let t = &p[ihl..];
        let h = usize::from(t[12] >> 4) * 4;
        if h < 20 || h > t.len() || word(t, 2) != 80 || tcp_sum(&p[12..16], &ip, t) != 0 {
            return None;
        }
        let peer = Peer {
            mac: frame[6..12].try_into().ok()?,
            ip: p[12..16].try_into().ok()?,
            port: word(t, 0),
        };
        let seq = u32::from_be_bytes(t[4..8].try_into().ok()?);
        let ack = u32::from_be_bytes(t[8..12].try_into().ok()?);
        let f = t[13];
        let slot = self.sockets.iter().position(|s| {
            s.state != State::Closed && s.peer.ip == peer.ip && s.peer.port == peer.port
        });
        let i =
            if let Some(i) = slot {
                i
            } else {
                if f & RST != 0 {
                    return None;
                }
                if let Some(old) = self.tombs.iter().find(|old| {
                    old.until > now && old.peer.ip == peer.ip && old.peer.port == peer.port
                }) {
                    return Some(packet(
                        out,
                        ip,
                        peer,
                        old.send,
                        old.recv,
                        ACK,
                        0,
                        &[],
                        false,
                    ));
                }
                if f & (SYN | ACK | FIN) != SYN {
                    return Some(if f & ACK != 0 {
                        packet(out, ip, peer, ack, 0, RST, 0, &[], false)
                    } else {
                        packet(
                            out,
                            ip,
                            peer,
                            0,
                            seq.wrapping_add((t.len() - h) as u32 + u32::from(f & FIN != 0)),
                            RST | ACK,
                            0,
                            &[],
                            false,
                        )
                    });
                }
                // Malformed options never create a socket.
                let mut mss = 536;
                let mut options = &t[20..h];
                while let Some((&kind, tail)) = options.split_first() {
                    if kind == 0 {
                        break;
                    }
                    if kind == 1 {
                        options = tail;
                        continue;
                    }
                    let len = usize::from(*tail.first()?);
                    if len < 2 || len - 1 > tail.len() {
                        return None;
                    }
                    if kind == 2 {
                        if len != 4 {
                            return None;
                        }
                        mss = usize::from(word(tail, 1)).clamp(1, MSS);
                    }
                    options = &tail[len - 1..];
                }
                let i = self
                    .sockets
                    .iter()
                    .position(|s| s.state == State::Closed && !s.held)?;
                self.serial = self.serial.wrapping_add(64001);
                let s = &mut self.sockets[i];
                s.peer = peer;
                s.state = State::Syn;
                s.recv = seq.wrapping_add(1);
                s.send = (now as u32).wrapping_add(self.serial);
                s.pending = 0;
                s.flags = SYN | ACK;
                s.sent = false;
                s.used = 0;
                s.eof = false;
                s.held = false;
                s.served = false;
                s.window = usize::from(word(t, 14));
                s.mss = mss;
                s.last = now;
                s.retry = now;
                s.retries = 0;
                s.ack = false;
                self.stats.accepted += 1;
                return None;
            };
        let s = &mut self.sockets[i];
        if f & RST != 0 {
            if seq == s.recv {
                s.reset();
                self.stats.resets += 1;
            } else {
                s.ack = true;
            }
            return None;
        }
        if s.state == State::Syn {
            if f & SYN != 0 && seq.wrapping_add(1) == s.recv {
                s.sent = false;
                return None;
            }
            if f & ACK == 0 || ack != s.send.wrapping_add(1) || seq != s.recv {
                return None;
            }
            s.send = ack;
            s.flags = 0;
            s.state = State::Open;
        } else if f & SYN != 0 {
            s.ack = true;
            return None;
        }
        // Validate sequence before accepting ACK/window/RST state changes.
        let delta = seq.wrapping_sub(s.recv) as i32;
        let data = &t[h..];
        if delta > 0
            || (delta < 0 && data.len() as i64 + i64::from(f & FIN != 0) < -i64::from(delta))
        {
            s.ack = true;
            return None;
        }
        if f & ACK == 0 {
            return None;
        }
        let advance = ack.wrapping_sub(s.send) as i32;
        if advance < 0 || advance as u32 > s.flight() {
            s.ack = true;
            return None;
        }
        // Unsent queued data cannot already have been acknowledged.
        if advance > 0 && !s.sent && s.flight() > 0 {
            return None;
        }
        s.window = usize::from(word(t, 14));
        s.last = now;
        if advance > 0 {
            if s.flags & FIN != 0 {
                s.flags = 0;
                s.send = ack;
                s.state = State::TimeWait;
            } else {
                let n = advance as usize;
                s.tx.copy_within(n..s.pending, 0);
                s.pending -= n;
                s.send = ack;
                if s.pending == 0 {
                    s.flags = 0;
                }
            }
            s.retries = 0;
            s.retry = now + 1_000_000;
        }
        let skip = if delta < 0 {
            (-i64::from(delta)) as usize
        } else {
            0
        };
        let available = data.len().saturating_sub(skip);
        let n = available.min(RX - s.used);
        if n != 0 && !s.eof {
            s.rx[s.used..s.used + n].copy_from_slice(&data[skip..skip + n]);
            s.used += n;
            s.recv = s.recv.wrapping_add(n as u32);
        }
        if f & FIN != 0 && !s.eof && s.used < RX && skip <= data.len() && n == available {
            s.recv = s.recv.wrapping_add(1);
            s.eof = true;
        }
        if !data.is_empty() || f & FIN != 0 {
            s.ack = true;
        }
        if s.state == State::TimeWait && s.eof {
            let n = packet(
                out,
                ip,
                s.peer,
                s.send,
                s.recv,
                ACK,
                (RX - s.used) as u16,
                &[],
                false,
            );
            self.bury(i, now);
            return Some(n);
        }
        None
    }
    pub fn poll(&mut self, ip: [u8; 4], now: u64, out: &mut [u8; FRAME]) -> Option<usize> {
        for i in 0..SLOTS {
            let s = &mut self.sockets[i];
            if s.state == State::Closed {
                continue;
            }
            if s.state == State::Reset {
                let n = packet(out, ip, s.peer, s.send, s.recv, RST | ACK, 0, &[], false);
                s.reset();
                return Some(n);
            }
            if now.saturating_sub(s.last) > 30_000_000 || s.retries >= 8 {
                s.reset();
                self.stats.timeouts += 1;
                continue;
            }
            if s.state == State::Closing && s.flight() == 0 {
                s.flags = FIN | ACK;
                s.sent = false;
                s.retries = 0;
            }
            if s.ack {
                s.ack = false;
                return Some(packet(
                    out,
                    ip,
                    s.peer,
                    s.send.wrapping_add(if s.sent { s.flight() } else { 0 }),
                    s.recv,
                    ACK,
                    (RX - s.used) as u16,
                    &[],
                    false,
                ));
            }
            if s.flight() != 0 && (!s.sent || now >= s.retry) {
                if s.sent {
                    self.stats.retransmits += 1;
                    s.retries += 1;
                }
                s.sent = true;
                s.retry = now + (1_000_000u64 << s.retries.min(2));
                return Some(packet(
                    out,
                    ip,
                    s.peer,
                    s.send,
                    s.recv,
                    s.flags,
                    (RX - s.used) as u16,
                    &s.tx[..s.pending],
                    s.flags & SYN != 0,
                ));
            }
            // Persist probe elicits a fresh window ACK if its update was lost.
            if s.state == State::Open && s.window == 0 && s.flight() == 0 && now >= s.retry {
                s.retry = now + 1_000_000;
                return Some(packet(
                    out,
                    ip,
                    s.peer,
                    s.send.wrapping_sub(1),
                    s.recv,
                    ACK,
                    (RX - s.used) as u16,
                    &[0],
                    false,
                ));
            }
        }
        None
    }
}
fn tcp_sum(src: &[u8], dst: &[u8], tcp: &[u8]) -> u16 {
    let mut pseudo = [0u8; 12];
    pseudo[..4].copy_from_slice(src);
    pseudo[4..8].copy_from_slice(dst);
    pseudo[9] = 6;
    put(&mut pseudo, 10, tcp.len() as u16);
    let mut sum = u32::from(!checksum(&pseudo)) + u32::from(!checksum(tcp));
    while sum >> 16 != 0 {
        sum = (sum & 65535) + (sum >> 16);
    }
    !(sum as u16)
}
#[allow(clippy::too_many_arguments)]
fn packet(
    out: &mut [u8; FRAME],
    ip: [u8; 4],
    peer: Peer,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    data: &[u8],
    syn: bool,
) -> usize {
    let h = if syn { 24 } else { 20 };
    let n = 14 + 20 + h + data.len();
    out[..n].fill(0);
    out[..6].copy_from_slice(&peer.mac);
    out[6..12].copy_from_slice(&MAC);
    put(out, 12, 0x0800);
    out[14] = 0x45;
    put(out, 16, (n - 14) as u16);
    out[22] = 64;
    out[23] = 6;
    out[26..30].copy_from_slice(&ip);
    out[30..34].copy_from_slice(&peer.ip);
    let c = checksum(&out[14..34]);
    put(out, 24, c);
    let t = &mut out[34..n];
    put(t, 0, 80);
    put(t, 2, peer.port);
    t[4..8].copy_from_slice(&seq.to_be_bytes());
    t[8..12].copy_from_slice(&ack.to_be_bytes());
    t[12] = (h as u8 / 4) << 4;
    t[13] = flags;
    put(t, 14, window);
    if syn {
        t[20..24].copy_from_slice(&[2, 4, (MSS >> 8) as u8, MSS as u8]);
    }
    t[h..].copy_from_slice(data);
    let c = tcp_sum(&ip, &peer.ip, t);
    put(t, 16, c);
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    const LOCAL: [u8; 4] = [10, 0, 0, 2];
    const CLIENT: [u8; 4] = [10, 0, 0, 1];
    fn incoming(seq: u32, ack: u32, flags: u8, data: &[u8]) -> ([u8; FRAME], usize) {
        let mut b = [0; FRAME];
        let n = packet(
            &mut b,
            CLIENT,
            Peer {
                mac: MAC,
                ip: LOCAL,
                port: 80,
            },
            seq,
            ack,
            flags,
            4096,
            data,
            false,
        );
        put(&mut b, 34, 40000);
        put(&mut b, 50, 0);
        let c = tcp_sum(&CLIENT, &LOCAL, &b[34..n]);
        put(&mut b, 50, c);
        (b, n)
    }
    fn input(s: &mut Stack, seq: u32, ack: u32, flags: u8, data: &[u8], now: u64) -> Option<usize> {
        let (b, n) = incoming(seq, ack, flags, data);
        s.ingest(&b[..n], LOCAL, now, &mut [0; FRAME])
    }
    fn open(s: &mut Stack) -> u32 {
        input(s, 100, 0, SYN, &[], 0);
        let mut b = [0; FRAME];
        s.poll(LOCAL, 1, &mut b).unwrap();
        assert_eq!(b[47], SYN | ACK);
        let seq = u32::from_be_bytes(b[38..42].try_into().unwrap());
        input(s, 101, seq.wrapping_add(1), ACK, &[], 2);
        seq.wrapping_add(1)
    }
    #[test]
    fn duplicates_partial_ack_backpressure_and_close() {
        let mut s = Stack::new();
        let seq = open(&mut s);
        input(&mut s, 101, seq, ACK, b"GET", 3);
        input(&mut s, 101, seq, ACK, b"GET", 4);
        assert_eq!(s.buffered(0), b"GET");
        assert_eq!(s.ready(), Some(0));
        s.hold(0);
        s.consume(0, 3);
        assert_eq!(s.write(0, b"response"), 8);
        let mut b = [0; FRAME];
        s.poll(LOCAL, 5, &mut b);
        let n = s.poll(LOCAL, 6, &mut b).unwrap();
        assert_eq!(&b[54..n], b"response");
        input(&mut s, 104, seq + 3, ACK, &[], 7);
        assert_eq!(s.sockets[0].pending, 5);
        input(&mut s, 104, seq + 8, ACK | FIN, &[], 8);
        assert!(s.eof(0));
        assert!(s.flushed(0));
        s.release(0);
        s.poll(LOCAL, 9, &mut b);
        let n = s.poll(LOCAL, 10, &mut b).unwrap();
        assert_eq!(n, 54);
        assert_eq!(b[47], FIN | ACK);
        input(&mut s, 105, seq + 9, ACK, &[], 11);
        assert!(!s.alive(0));
        assert_eq!(s.stats.resets, 0);
    }
    #[test]
    fn timeout_retransmission_and_untrusted_acks() {
        let mut s = Stack::new();
        let seq = open(&mut s);
        s.hold(0);
        assert_eq!(s.write(0, b"abc"), 3);
        let mut b = [0; FRAME];
        s.poll(LOCAL, 10, &mut b);
        input(&mut s, 101, seq + 4, ACK, &[], 11);
        assert_eq!(s.sockets[0].pending, 3);
        s.poll(LOCAL, 12, &mut b);
        s.poll(LOCAL, 1_000_011, &mut b);
        assert_eq!(s.stats.retransmits, 1);
        input(&mut s, 9999, seq + 3, ACK, &[], 1_000_012);
        assert!(!s.flushed(0));
        input(&mut s, 101, seq + 3, ACK, &[], 1_000_013);
        assert!(s.flushed(0));
        input(&mut s, 9999, 0, RST, &[], 1_000_014);
        assert!(s.alive(0));
        input(&mut s, 101, 0, RST, &[], 1_000_015);
        assert!(!s.alive(0));
    }
    #[test]
    fn rx_window_and_wraparound() {
        let mut s = Stack::new();
        let seq = open(&mut s);
        s.sockets[0].recv = u32::MAX - 100;
        let first = [42; MSS];
        input(&mut s, u32::MAX - 100, seq, ACK, &first, 3);
        input(
            &mut s,
            (u32::MAX - 100).wrapping_add(MSS as u32),
            seq,
            ACK,
            &first,
            4,
        );
        assert_eq!(s.buffered(0).len(), RX);
        let end = s.sockets[0].recv;
        input(&mut s, end, seq, ACK | FIN, b"abc", 5);
        assert!(!s.eof(0));
        s.consume(0, 100);
        input(&mut s, end, seq, ACK | FIN, b"abc", 6);
        assert!(s.eof(0));
    }
    #[test]
    fn empty_read_does_not_generate_ack_traffic() {
        let mut s = Stack::new();
        open(&mut s);
        assert_eq!(s.read(0, &mut [0; 64]), 0);
        assert_eq!(s.poll(LOCAL, 10, &mut [0; FRAME]), None);
    }
    #[test]
    fn checksum_and_options_reject_before_allocating() {
        let mut s = Stack::new();
        let (mut b, n) = incoming(100, 0, SYN, &[]);
        b[40] ^= 1;
        assert!(s.ingest(&b[..n], LOCAL, 0, &mut [0; FRAME]).is_none());
        assert_eq!(s.stats.accepted, 0);
        let (mut b, n) = incoming(100, 0, SYN, &[]);
        for len in 0..n {
            assert!(s.ingest(&b[..len], LOCAL, 0, &mut [0; FRAME]).is_none());
        }
        b[46] = 0x60;
        b[54..58].copy_from_slice(&[2, 1, 0, 0]); // Invalid MSS option length.
        put(&mut b, 16, 44);
        put(&mut b, 24, 0);
        let c = checksum(&b[14..34]);
        put(&mut b, 24, c);
        put(&mut b, 50, 0);
        let c = tcp_sum(&CLIENT, &LOCAL, &b[34..58]);
        put(&mut b, 50, c);
        assert!(s.ingest(&b[..58], LOCAL, 0, &mut [0; FRAME]).is_none());
        assert_eq!(s.stats.accepted, 0);
    }
    #[test]
    fn application_abort_sends_reset_before_reusing_slot() {
        let mut s = Stack::new();
        open(&mut s);
        s.hold(0);
        s.abort(0);
        s.release(0);
        let mut b = [0; FRAME];
        s.poll(LOCAL, 10, &mut b).unwrap();
        assert_eq!(b[47], RST | ACK);
        assert!(!s.alive(0));
    }
}
