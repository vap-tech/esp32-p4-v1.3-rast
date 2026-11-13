//! Polled smoltcp adapter. Static SRAM queues; no allocator or async runtime.
pub use smoltcp::iface::SocketStorage as Storage;
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet, SocketStorage},
    phy::Device,
    socket::tcp::{Socket, SocketBuffer, State},
    time::{Duration, Instant},
    wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address},
};
pub const SLOTS: usize = 4;
pub const RX: usize = 6 * 1024;
pub const TX: usize = 16384;
pub const MSS: usize = 1460;
/// HTTP header limit is independent of the receive window.
pub const HEADER: usize = 2048;
pub struct Buffers {
    rx: [[u8; RX]; SLOTS],
    tx: [[u8; TX]; SLOTS],
}
impl Buffers {
    pub const fn new() -> Self {
        Self {
            rx: [[0; RX]; SLOTS],
            tx: [[0; TX]; SLOTS],
        }
    }
}
impl Default for Buffers {
    fn default() -> Self {
        Self::new()
    }
}
#[derive(Default, Debug)]
pub struct Stats {
    pub accepted: u32,
    pub completed: u32,
    pub tx_queued: u64,
    pub rx_consumed: u64,
}
pub struct Stack<'a> {
    iface: Interface,
    sockets: SocketSet<'a>,
    handles: [SocketHandle; SLOTS],
    held: [bool; SLOTS],
    served: [bool; SLOTS],
    counted: [bool; SLOTS],
    pub stats: Stats,
}
impl<'a> Stack<'a> {
    pub fn new(
        storage: &'a mut [SocketStorage<'a>; SLOTS],
        buffers: &'a mut Buffers,
        mac: [u8; 6],
        now: u64,
    ) -> Self {
        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
        config.random_seed =
            now ^ u64::from_be_bytes([mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], 0x19, 0x83]);
        let iface = Interface::new(config, &mut CapabilitiesOnly, instant(now));
        // Previous synchronous server has dropped; no socket can retain these buffers.
        for entry in storage.iter_mut() {
            *entry = SocketStorage::EMPTY;
        }
        let mut sockets = SocketSet::new(&mut storage[..]);
        let mut handles = [None; SLOTS];
        for (i, (rx, tx)) in buffers.rx.iter_mut().zip(buffers.tx.iter_mut()).enumerate() {
            let mut socket = Socket::new(
                SocketBuffer::new(&mut rx[..]),
                SocketBuffer::new(&mut tx[..]),
            );
            socket.set_timeout(Some(Duration::from_secs(30)));
            socket.set_ack_delay(None);
            socket.set_nagle_enabled(false);
            socket.listen(80).unwrap();
            handles[i] = Some(sockets.add(socket));
        }
        Self {
            iface,
            sockets,
            handles: handles.map(Option::unwrap),
            held: [false; SLOTS],
            served: [false; SLOTS],
            counted: [false; SLOTS],
            stats: Stats::default(),
        }
    }
    pub fn configure(
        &mut self,
        ip: [u8; 4],
        mask: [u8; 4],
        router: [u8; 4],
    ) -> Result<(), &'static str> {
        let bits = u32::from_be_bytes(mask);
        let prefix = bits.leading_ones() as u8;
        if bits
            != if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            }
        {
            return Err("invalid netmask");
        }
        self.iface.update_ip_addrs(|addrs| {
            addrs.clear();
            addrs
                .push(IpCidr::new(IpAddress::Ipv4(Ipv4Address::from(ip)), prefix))
                .unwrap();
        });
        if router != [0; 4] {
            self.iface
                .routes_mut()
                .add_default_ipv4_route(Ipv4Address::from(router))
                .map_err(|_| "route capacity")?;
        }
        Ok(())
    }
    pub fn poll<D: Device>(&mut self, now: u64, device: &mut D) {
        self.iface.poll(instant(now), device, &mut self.sockets);
        for i in 0..SLOTS {
            let socket = self.sockets.get_mut::<Socket>(self.handles[i]);
            if !self.counted[i] && !matches!(socket.state(), State::Listen | State::Closed) {
                self.counted[i] = true;
                self.stats.accepted += 1;
            }
            // Use smoltcp's listener lifecycle: is_open excludes Closed and
            // TimeWait, and listen() explicitly permits both. Live FIN states
            // retain their socket until the close handshake finishes.
            if !socket.is_open() && !self.held[i] {
                socket.listen(80).unwrap();
                self.served[i] = false;
                self.counted[i] = false;
            }
        }
    }
    fn socket(&self, i: usize) -> &Socket<'a> {
        self.sockets.get(self.handles[i])
    }
    fn socket_mut(&mut self, i: usize) -> &mut Socket<'a> {
        self.sockets.get_mut(self.handles[i])
    }
    pub fn request_pending(&self, i: usize) -> bool {
        !self.held[i] && !self.served[i] && self.socket(i).can_recv()
    }
    pub fn buffered(&mut self, i: usize) -> &[u8] {
        self.socket_mut(i).peek(HEADER).unwrap_or(&[])
    }
    pub fn consume(&mut self, i: usize, n: usize) {
        let mut remaining = n;
        while remaining != 0 {
            let used = self
                .socket_mut(i)
                .recv(|data| {
                    let n = data.len().min(remaining);
                    (n, n)
                })
                .unwrap();
            assert!(used != 0);
            remaining -= used;
        }
        self.stats.rx_consumed += n as u64;
    }
    pub fn hold(&mut self, i: usize) {
        self.held[i] = true;
    }
    pub fn release(&mut self, i: usize) {
        self.held[i] = false;
        self.served[i] = true;
        self.socket_mut(i).close();
        self.stats.completed += 1;
    }
    pub fn abort(&mut self, i: usize) {
        self.socket_mut(i).abort();
    }
    pub fn alive(&self, i: usize) -> bool {
        self.socket(i).may_send()
    }
    pub fn eof(&self, i: usize) -> bool {
        !self.socket(i).may_recv() && self.socket(i).recv_queue() == 0
    }
    pub fn read(&mut self, i: usize, out: &mut [u8]) -> usize {
        let n = self.socket_mut(i).recv_slice(out).unwrap_or(0);
        self.stats.rx_consumed += n as u64;
        n
    }
    pub fn write(&mut self, i: usize, bytes: &[u8]) -> usize {
        let n = self.socket_mut(i).send_slice(bytes).unwrap_or(0);
        self.stats.tx_queued += n as u64;
        n
    }
    pub fn flushed(&self, i: usize) -> bool {
        self.socket(i).send_queue() == 0
    }
}
impl Drop for Stack<'_> {
    fn drop(&mut self) {
        // SocketStorage must not retain references after its SRAM lease is returned.
        for handle in self.handles {
            let _ = self.sockets.remove(handle);
        }
    }
}
fn instant(us: u64) -> Instant {
    Instant::from_micros(us.min(i64::MAX as u64) as i64)
}

// Interface construction only needs the frame format and MTU, not I/O.
struct CapabilitiesOnly;
impl smoltcp::phy::Device for CapabilitiesOnly {
    type RxToken<'b> = NoToken;
    type TxToken<'b> = NoToken;
    fn capabilities(&self) -> smoltcp::phy::DeviceCapabilities {
        capabilities()
    }
    fn receive(&mut self, _: Instant) -> Option<(NoToken, NoToken)> {
        None
    }
    fn transmit(&mut self, _: Instant) -> Option<NoToken> {
        None
    }
}
struct NoToken;
impl smoltcp::phy::RxToken for NoToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, _: F) -> R {
        unreachable!()
    }
}
impl smoltcp::phy::TxToken for NoToken {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, _: usize, _: F) -> R {
        unreachable!()
    }
}
pub fn capabilities() -> smoltcp::phy::DeviceCapabilities {
    let mut caps = smoltcp::phy::DeviceCapabilities::default();
    caps.medium = smoltcp::phy::Medium::Ethernet;
    caps.max_transmission_unit = 1514;
    caps.max_burst_size = Some(8);
    caps
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use smoltcp::phy::{DeviceCapabilities, RxToken, TxToken};
    use std::{collections::VecDeque, vec::Vec};
    #[derive(Default)]
    struct Wire {
        incoming: VecDeque<Vec<u8>>,
        outgoing: VecDeque<Vec<u8>>,
    }
    struct Rx(Vec<u8>);
    struct Tx<'a>(&'a mut VecDeque<Vec<u8>>);
    impl RxToken for Rx {
        fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
            f(&self.0)
        }
    }
    impl TxToken for Tx<'_> {
        fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, n: usize, f: F) -> R {
            let mut b = std::vec![0;n];
            let r = f(&mut b);
            self.0.push_back(b);
            r
        }
    }
    impl Device for Wire {
        type RxToken<'b> = Rx;
        type TxToken<'b> = Tx<'b>;
        fn capabilities(&self) -> DeviceCapabilities {
            capabilities()
        }
        fn receive(&mut self, _: Instant) -> Option<(Rx, Tx<'_>)> {
            self.incoming
                .pop_front()
                .map(|p| (Rx(p), Tx(&mut self.outgoing)))
        }
        fn transmit(&mut self, _: Instant) -> Option<Tx<'_>> {
            Some(Tx(&mut self.outgoing))
        }
    }
    fn data_packet(frame: &[u8]) -> bool {
        if frame.len() < 54 || frame[12..14] != [8, 0] || frame[23] != 6 {
            return false;
        }
        let ih = (frame[14] & 15) as usize * 4;
        let th = (frame[14 + ih + 12] >> 4) as usize * 4;
        u16::from_be_bytes([frame[16], frame[17]]) as usize > ih + th
    }
    #[test]
    fn sliding_window_recovers_loss_and_reordering_without_duplicate_stream_bytes() {
        for inject_loss in [false, true] {
            let mut buffers = Buffers::new();
            let mut storage = [const { SocketStorage::EMPTY }; SLOTS];
            let mut server = Stack::new(&mut storage, &mut buffers, super::super::MAC, 1);
            server
                .configure([192, 0, 2, 1], [255, 255, 255, 0], [0; 4])
                .unwrap();
            let mut sw = Wire::default();
            let mut cw = Wire::default();
            let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress([
                2, 0, 0, 0, 0, 2,
            ])));
            config.random_seed = 42;
            let mut iface = Interface::new(config, &mut cw, instant(1));
            iface.update_ip_addrs(|a| {
                a.push(IpCidr::new(IpAddress::v4(192, 0, 2, 2), 24))
                    .unwrap()
            });
            let mut client_rx = std::vec![0u8;65536];
            let mut client_tx = std::vec![0u8;65536];
            let mut client_storage = [SocketStorage::EMPTY];
            let mut sockets = SocketSet::new(&mut client_storage[..]);
            let mut client = Socket::new(
                SocketBuffer::new(&mut client_rx[..]),
                SocketBuffer::new(&mut client_tx[..]),
            );
            client.set_ack_delay(None);
            client.set_nagle_enabled(false);
            client
                .connect(iface.context(), (Ipv4Address::new(192, 0, 2, 1), 80), 50000)
                .unwrap();
            let handle = sockets.add(client);
            let expected: Vec<u8> = (0..65539).map(|i| i as u8).collect();
            let (mut sent, mut received, mut slot, mut request_sent, mut dropped, mut max_burst) =
                (0, Vec::new(), None, false, false, 0);
            for tick in 1..30000 {
                let now = tick * 1000;
                let c = sockets.get_mut::<Socket>(handle);
                if c.can_send() && !request_sent {
                    c.send_slice(b"request").unwrap();
                    request_sent = true;
                }
                iface.poll(instant(now), &mut cw, &mut sockets);
                sw.incoming.extend(cw.outgoing.drain(..));
                server.poll(now, &mut sw);
                if slot.is_none() {
                    slot = (0..SLOTS).find(|&i| server.request_pending(i));
                    if let Some(i) = slot {
                        server.hold(i);
                        server.consume(i, 7);
                    }
                }
                if let Some(i) = slot {
                    if sent < expected.len() {
                        sent += server.write(i, &expected[sent..]);
                    }
                }
                server.poll(now, &mut sw);
                let count = sw.outgoing.iter().filter(|p| data_packet(p)).count();
                max_burst = max_burst.max(count);
                let mut packets: Vec<_> = sw.outgoing.drain(..).collect();
                if inject_loss && !dropped && count >= 2 {
                    let i = packets.iter().position(|p| data_packet(p)).unwrap();
                    packets.remove(i);
                    dropped = true;
                    packets.reverse(); // Exercise out-of-order reception as well as retransmission.
                }
                cw.incoming.extend(packets);
                let c = sockets.get_mut::<Socket>(handle);
                while c.can_recv() {
                    c.recv(|b| {
                        received.extend_from_slice(b);
                        (b.len(), ())
                    })
                    .unwrap();
                }
                if received.len() == expected.len() && slot.is_some_and(|i| server.flushed(i)) {
                    break;
                }
            }
            assert_eq!(received, expected, "loss={inject_loss}");
            assert!(
                max_burst > 1,
                "must send several segments before receiving ACKs"
            );
            assert!(!inject_loss || dropped);
            // Exercise the receive window too: stop consuming until it fills,
            // then reopen it and verify that the stream resumes without loss.
            let i = slot.unwrap();
            let mut uploaded = 0;
            let mut upload = Vec::new();
            let mut saw_full_window = false;
            for tick in 30000..60000 {
                let now = tick * 1000;
                let c = sockets.get_mut::<Socket>(handle);
                if uploaded < expected.len() {
                    uploaded += c.send_slice(&expected[uploaded..]).unwrap_or(0);
                }
                iface.poll(instant(now), &mut cw, &mut sockets);
                sw.incoming.extend(cw.outgoing.drain(..));
                server.poll(now, &mut sw);
                saw_full_window |= server.socket(i).recv_queue() == RX;
                if tick >= 30200 {
                    let mut block = [0; 1003];
                    let n = server.read(i, &mut block);
                    upload.extend_from_slice(&block[..n]);
                }
                cw.incoming.extend(sw.outgoing.drain(..));
                if upload.len() == expected.len() {
                    break;
                }
            }
            assert!(saw_full_window, "receiver must apply backpressure");
            assert_eq!(upload, expected);
            // A fixed pool must keep accepting after several close handshakes.
            // Follow the public smoltcp is_open/listen lifecycle, including TIME_WAIT.
            let mut active = i;
            let mut now = 60_000_000;
            for round in 0..12u16 {
                server.release(active);
                sockets.get_mut::<Socket>(handle).close();
                for _ in 0..2000 {
                    now += 1000;
                    iface.poll(instant(now), &mut cw, &mut sockets);
                    sw.incoming.extend(cw.outgoing.drain(..));
                    server.poll(now, &mut sw);
                    cw.incoming.extend(sw.outgoing.drain(..));
                    if !sockets.get::<Socket>(handle).is_open()
                        && server.socket(active).is_listening()
                    {
                        break;
                    }
                }
                assert!(!sockets.get::<Socket>(handle).is_open());
                assert!(server.socket(active).is_listening());
                sockets
                    .get_mut::<Socket>(handle)
                    .connect(
                        iface.context(),
                        (Ipv4Address::new(192, 0, 2, 1), 80),
                        50001 + round,
                    )
                    .unwrap();
                let mut queued = false;
                let mut next = None;
                for _ in 0..2000 {
                    now += 1000;
                    let c = sockets.get_mut::<Socket>(handle);
                    if c.can_send() && !queued {
                        c.send_slice(b"again").unwrap();
                        queued = true;
                    }
                    iface.poll(instant(now), &mut cw, &mut sockets);
                    sw.incoming.extend(cw.outgoing.drain(..));
                    server.poll(now, &mut sw);
                    cw.incoming.extend(sw.outgoing.drain(..));
                    next = (0..SLOTS).find(|&i| server.request_pending(i));
                    if next.is_some() {
                        break;
                    }
                }
                active = next.expect("listener must be reusable");
                server.hold(active);
                let mut message = [0; 5];
                assert_eq!(server.read(active, &mut message), 5);
                assert_eq!(&message, b"again");
            }
        }
    }
}
