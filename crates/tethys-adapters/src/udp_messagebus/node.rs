//! One node on the UDP message bus: the socket work `FUdpMessageProcessor` does in UE.
//!
//! The node finds other nodes through their multicast Hello beacons, or through Ping segments
//! from an editor started with `-UDPMESSAGING_TRANSPORT_STATIC=<our endpoint>`. It reassembles
//! their messages, acknowledging every segment as UE's sender expects, and splits its own
//! messages into segments, resending any the other side doesn't acknowledge.
//!
//! It's single-threaded and polled: call [`Node::poll`] regularly, at least every few hundred
//! milliseconds, or peers time it out and stop sending.

use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};

use super::message::Message;
use super::wire::{
    DataChunk, Guid, Header, Reader, SegmentType, Writer, read_ack_segments, read_retransmit,
    write_ack_segments,
};

/// The protocol version we speak. UE 5.8 is 18 and accepts 10–18; the layouts we use are the
/// same from 16 on.
pub const PROTOCOL_VERSION: u8 = 18;
const MIN_PROTOCOL_VERSION: u8 = 16;
pub const DEFAULT_MULTICAST: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(230, 0, 0, 1), 6666);

/// Payload bytes per data segment. UE's default packet limit is 1130 bytes including IP and UDP
/// headers; this stays under it with our segment headers.
const SEGMENT_PAYLOAD: usize = 1000;
const HELLO_INTERVAL: Duration = Duration::from_secs(1);
const RESEND_INTERVAL: Duration = Duration::from_millis(200);
const SEND_ATTEMPTS: u32 = 25;
const PEER_TIMEOUT: Duration = Duration::from_secs(10);
const REASSEMBLY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum Event {
    NodeFound {
        node: Guid,
        endpoint: SocketAddr,
        protocol_version: u8,
    },
    NodeLost(Guid),
    Message {
        node: Guid,
        message: Message,
    },
    /// A message arrived whole but its body couldn't be decoded.
    BadMessage {
        node: Guid,
        error: String,
    },
    /// One of our messages was never acknowledged.
    SendFailed {
        node: Guid,
        type_path: String,
    },
}

struct Peer {
    endpoint: SocketAddr,
    protocol_version: u8,
    last_seen: Instant,
}

struct Reassembly {
    size: usize,
    segments: Vec<Option<Vec<u8>>>,
    received: usize,
    last_segment: Instant,
}

struct Outbound {
    node: Guid,
    message_id: i32,
    type_path: String,
    /// Whole datagrams, one per segment.
    datagrams: Vec<Vec<u8>>,
    acked: Vec<bool>,
    last_sent: Instant,
    attempts: u32,
}

pub struct Node {
    id: Guid,
    unicast: UdpSocket,
    multicast: Option<UdpSocket>,
    multicast_group: SocketAddrV4,
    peers: HashMap<Guid, Peer>,
    reassembly: HashMap<(Guid, i32), Reassembly>,
    recently_delivered: VecDeque<(Guid, i32)>,
    outbound: Vec<Outbound>,
    last_message_id: i32,
    next_hello: Instant,
    events: Vec<Event>,
    /// When set, every reassembled message is written here as a `.bin` file (test fixtures).
    pub dump_dir: Option<PathBuf>,
    dump_count: u32,
}

impl Node {
    /// Binds `unicast` (port 0 for any) and, when `join_multicast` is set, the multicast group
    /// alongside the editor (the port is shared).
    pub fn bind(unicast: SocketAddrV4, join_multicast: bool) -> std::io::Result<Node> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        // UE's own receive buffer size; a test list arrives as a burst of segments.
        let _ = socket.set_recv_buffer_size(2 * 1024 * 1024);
        socket.bind(&SocketAddr::V4(unicast).into())?;
        socket.set_nonblocking(true)?;
        let _ = socket.set_multicast_ttl_v4(1);
        let unicast: UdpSocket = socket.into();

        let multicast = if join_multicast {
            Some(join_group(DEFAULT_MULTICAST)?)
        } else {
            None
        };
        Ok(Node {
            id: Guid::new_random(),
            unicast,
            multicast,
            multicast_group: DEFAULT_MULTICAST,
            peers: HashMap::new(),
            reassembly: HashMap::new(),
            recently_delivered: VecDeque::new(),
            outbound: Vec::new(),
            last_message_id: 0,
            next_hello: Instant::now(),
            events: Vec::new(),
            dump_dir: None,
            dump_count: 0,
        })
    }

    pub fn id(&self) -> Guid {
        self.id
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.unicast.local_addr()
    }

    pub fn peers(&self) -> impl Iterator<Item = (Guid, SocketAddr)> + '_ {
        self.peers.iter().map(|(id, p)| (*id, p.endpoint))
    }

    /// True while any of our messages still waits for acknowledgement.
    pub fn is_sending(&self) -> bool {
        !self.outbound.is_empty()
    }

    /// Introduces us to a node at a known endpoint, as UE does for static endpoints.
    pub fn ping(&mut self, endpoint: SocketAddr) {
        let mut w = self.header(Guid::ZERO, PROTOCOL_VERSION, SegmentType::Ping);
        w.guid(self.id).u8(PROTOCOL_VERSION);
        self.send_to(&w.buf, endpoint);
    }

    /// Sends `message` to one node. Its recipients field says which endpoints on that node get it;
    /// leave it empty to publish to the node's subscribers.
    pub fn send(&mut self, node: Guid, message: &Message) {
        let Some(peer) = self.peers.get(&node) else {
            self.events.push(Event::SendFailed {
                node,
                type_path: message.type_path.clone(),
            });
            return;
        };
        let (endpoint, version) = (peer.endpoint, peer.protocol_version);
        let payload = message.encode();
        self.last_message_id = self.last_message_id.wrapping_add(1).max(1);
        let message_id = self.last_message_id;
        let total = payload.len().div_ceil(SEGMENT_PAYLOAD).max(1);
        let datagrams: Vec<Vec<u8>> = (0..total)
            .map(|i| {
                let start = i * SEGMENT_PAYLOAD;
                let end = (start + SEGMENT_PAYLOAD).min(payload.len());
                let mut w = self.header(node, version, SegmentType::Data);
                DataChunk {
                    message_id,
                    message_size: payload.len() as i64,
                    message_flags: message.flags,
                    segment_number: i as u32,
                    segment_offset: start as u64,
                    total_segments: total as u32,
                    sequence: 0,
                    data: payload[start..end].to_vec(),
                }
                .write(&mut w);
                w.buf
            })
            .collect();
        for d in &datagrams {
            self.send_to(d, endpoint);
        }
        self.outbound.push(Outbound {
            node,
            message_id,
            type_path: message.type_path.clone(),
            acked: vec![false; datagrams.len()],
            datagrams,
            last_sent: Instant::now(),
            attempts: 1,
        });
    }

    /// Sends `message` to every known node.
    pub fn publish(&mut self, message: &Message) {
        let nodes: Vec<Guid> = self.peers.keys().copied().collect();
        for node in nodes {
            self.send(node, message);
        }
    }

    /// Does the node's socket work for up to `timeout` and returns what happened. Returns
    /// early once there is something to report.
    pub fn poll(&mut self, timeout: Duration) -> Vec<Event> {
        let deadline = Instant::now() + timeout;
        loop {
            self.tick();
            let mut got_any = false;
            let mut buf = [0u8; 65536];
            loop {
                let received = match self.unicast.recv_from(&mut buf) {
                    Ok(r) => Some(r),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => None,
                    // Windows reports an ICMP "port unreachable" from an earlier send this way.
                    Err(e) if e.kind() == ErrorKind::ConnectionReset => continue,
                    Err(_) => None,
                };
                let received = received.or_else(|| {
                    self.multicast
                        .as_ref()
                        .and_then(|m| m.recv_from(&mut buf).ok())
                });
                let Some((n, from)) = received else { break };
                got_any = true;
                self.handle_datagram(&buf[..n], from);
            }
            if !self.events.is_empty() || Instant::now() >= deadline {
                return std::mem::take(&mut self.events);
            }
            if !got_any {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    fn header(&self, recipient: Guid, version: u8, kind: SegmentType) -> Writer {
        let mut w = Writer::default();
        Header {
            protocol_version: version,
            recipient,
            sender: self.id,
            segment_type: kind as u8,
        }
        .write(&mut w);
        w
    }

    fn send_to(&self, datagram: &[u8], to: SocketAddr) {
        // A dropped datagram is the protocol's normal case; resends cover it.
        let _ = self.unicast.send_to(datagram, to);
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if now >= self.next_hello {
            self.next_hello = now + HELLO_INTERVAL;
            let mut w = self.header(Guid::ZERO, PROTOCOL_VERSION, SegmentType::Hello);
            w.guid(self.id);
            if self.multicast.is_some() {
                self.send_to(&w.buf, SocketAddr::V4(self.multicast_group));
            }
            // Unicast too, so peers that found us by Ping (or can't hear multicast) keep us alive.
            for p in self.peers.values() {
                self.send_to(&w.buf, p.endpoint);
            }
        }

        let lost: Vec<Guid> = self
            .peers
            .iter()
            .filter(|(_, p)| now - p.last_seen > PEER_TIMEOUT)
            .map(|(id, _)| *id)
            .collect();
        for id in lost {
            self.peers.remove(&id);
            self.events.push(Event::NodeLost(id));
        }

        self.reassembly
            .retain(|_, r| now - r.last_segment < REASSEMBLY_TIMEOUT);

        let mut failed = Vec::new();
        for o in &mut self.outbound {
            if now - o.last_sent < RESEND_INTERVAL {
                continue;
            }
            if o.attempts >= SEND_ATTEMPTS {
                failed.push((o.node, o.message_id));
                continue;
            }
            let Some(peer) = self.peers.get(&o.node) else {
                failed.push((o.node, o.message_id));
                continue;
            };
            for (d, acked) in o.datagrams.iter().zip(&o.acked) {
                if !acked {
                    let _ = self.unicast.send_to(d, peer.endpoint);
                }
            }
            o.last_sent = now;
            o.attempts += 1;
        }
        for (node, id) in failed {
            if let Some(i) = self
                .outbound
                .iter()
                .position(|o| o.node == node && o.message_id == id)
            {
                let o = self.outbound.remove(i);
                self.events.push(Event::SendFailed {
                    node,
                    type_path: o.type_path,
                });
            }
        }
    }

    fn handle_datagram(&mut self, data: &[u8], from: SocketAddr) {
        let mut r = Reader::new(data);
        let Ok(header) = Header::read(&mut r) else {
            return;
        };
        let Some(kind) = SegmentType::from_u8(header.segment_type) else {
            return;
        };
        // UE sends Pings with a version 11 header so that any node can read them, and puts its
        // real version in the body.
        let version = if kind == SegmentType::Ping {
            let mut body = Reader::new(r.rest());
            let _ = body.guid();
            body.u8().unwrap_or(header.protocol_version)
        } else {
            header.protocol_version
        };
        if header.sender == self.id
            || (header.recipient.is_valid() && header.recipient != self.id)
            || version < MIN_PROTOCOL_VERSION
        {
            return;
        }

        let node = header.sender;
        let now = Instant::now();
        match self.peers.get_mut(&node) {
            Some(p) => {
                p.endpoint = from;
                p.last_seen = now;
            }
            None => {
                if kind == SegmentType::Bye {
                    return;
                }
                let protocol_version = version.min(PROTOCOL_VERSION);
                self.peers.insert(
                    node,
                    Peer {
                        endpoint: from,
                        protocol_version,
                        last_seen: now,
                    },
                );
                self.events.push(Event::NodeFound {
                    node,
                    endpoint: from,
                    protocol_version,
                });
            }
        }

        match kind {
            SegmentType::Data => self.handle_data(node, &mut r),
            SegmentType::Ping => {
                let version = version.min(PROTOCOL_VERSION);
                if let Some(p) = self.peers.get_mut(&node) {
                    p.protocol_version = version;
                }
                let mut w = self.header(node, version, SegmentType::Pong);
                w.guid(self.id);
                self.send_to(&w.buf, from);
            }
            SegmentType::AcknowledgeSegments => {
                if let Ok((id, segments)) = read_ack_segments(&mut r) {
                    if let Some(o) = self.outbound_mut(node, id) {
                        for s in segments {
                            if let Some(a) = o.acked.get_mut(s as usize) {
                                *a = true;
                            }
                        }
                    }
                    self.finish_outbound(node, id, false);
                }
            }
            SegmentType::Acknowledge => {
                if let Ok(id) = r.i32() {
                    self.finish_outbound(node, id, true);
                }
            }
            SegmentType::Retransmit => {
                if let Ok((id, segments)) = read_retransmit(&mut r)
                    && let Some(o) = self
                        .outbound
                        .iter()
                        .find(|o| o.node == node && o.message_id == id)
                {
                    for s in segments {
                        if let Some(d) = o.datagrams.get(s as usize) {
                            let _ = self.unicast.send_to(d, from);
                        }
                    }
                }
            }
            SegmentType::Abort | SegmentType::Timeout => {
                if let Ok(id) = r.i32() {
                    self.finish_outbound(node, id, true);
                }
            }
            SegmentType::Bye => {
                self.peers.remove(&node);
                self.events.push(Event::NodeLost(node));
            }
            SegmentType::Hello | SegmentType::Pong | SegmentType::Mesh | SegmentType::None => {}
        }
    }

    fn outbound_mut(&mut self, node: Guid, id: i32) -> Option<&mut Outbound> {
        self.outbound
            .iter_mut()
            .find(|o| o.node == node && o.message_id == id)
    }

    fn finish_outbound(&mut self, node: Guid, id: i32, all: bool) {
        self.outbound.retain(|o| {
            !(o.node == node && o.message_id == id && (all || o.acked.iter().all(|a| *a)))
        });
    }

    fn handle_data(&mut self, node: Guid, r: &mut Reader) {
        let Ok(chunk) = DataChunk::read(r) else {
            return;
        };
        let key = (node, chunk.message_id);
        let version = self
            .peers
            .get(&node)
            .map_or(PROTOCOL_VERSION, |p| p.protocol_version);
        let endpoint = match self.peers.get(&node) {
            Some(p) => p.endpoint,
            None => return,
        };

        // Acknowledge every segment, even repeats: the sender stalls on unacknowledged ones.
        let mut w = self.header(node, version.max(11), SegmentType::AcknowledgeSegments);
        write_ack_segments(&mut w, chunk.message_id, &[chunk.segment_number]);
        self.send_to(&w.buf, endpoint);

        if self.recently_delivered.contains(&key) {
            return;
        }
        let total = chunk.total_segments as usize;
        let size = chunk.message_size.max(0) as usize;
        if total == 0
            || total > 1 << 20
            || size > 256 << 20
            || chunk.segment_number as usize >= total
        {
            return;
        }
        let entry = self.reassembly.entry(key).or_insert_with(|| Reassembly {
            size,
            segments: vec![None; total],
            received: 0,
            last_segment: Instant::now(),
        });
        if entry.segments.len() != total {
            return;
        }
        entry.last_segment = Instant::now();
        let slot = &mut entry.segments[chunk.segment_number as usize];
        if slot.is_none() {
            *slot = Some(chunk.data);
            entry.received += 1;
        }
        if entry.received < total {
            return;
        }

        let done = self.reassembly.remove(&key).expect("present");
        let mut payload = Vec::with_capacity(done.size);
        for s in done.segments.into_iter().flatten() {
            payload.extend_from_slice(&s);
        }
        payload.truncate(done.size);

        let mut w = self.header(node, version, SegmentType::Acknowledge);
        w.i32(chunk.message_id);
        self.send_to(&w.buf, endpoint);
        self.recently_delivered.push_back(key);
        if self.recently_delivered.len() > 256 {
            self.recently_delivered.pop_front();
        }

        let decoded = Message::decode(&payload);
        self.dump(
            &payload,
            decoded
                .as_ref()
                .map(|m| m.type_name())
                .unwrap_or("Undecodable"),
        );
        match decoded {
            Ok(message) => self.events.push(Event::Message { node, message }),
            Err(e) => self.events.push(Event::BadMessage {
                node,
                error: e.to_string(),
            }),
        }
    }

    fn dump(&mut self, payload: &[u8], type_name: &str) {
        let Some(dir) = &self.dump_dir else { return };
        self.dump_count += 1;
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(
            dir.join(format!("{:04}-{type_name}.bin", self.dump_count)),
            payload,
        );
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let mut w = self.header(Guid::ZERO, PROTOCOL_VERSION, SegmentType::Bye);
        w.guid(self.id);
        for p in self.peers.values() {
            self.send_to(&w.buf, p.endpoint);
        }
        if self.multicast.is_some() {
            self.send_to(&w.buf, SocketAddr::V4(self.multicast_group));
        }
    }
}

/// A socket that receives the bus's multicast traffic. The port is shared with any editor on
/// this machine, which binds it the same way.
fn join_group(group: SocketAddrV4) -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, group.port())).into())?;
    socket.join_multicast_v4(group.ip(), &Ipv4Addr::UNSPECIFIED)?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}
