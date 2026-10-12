//! Byte-level encoding: UE's `FArchive` conventions (little-endian, `FGuid`, `FString`) and the
//! UDP messaging segment formats (`UdpMessageSegment.h`).

use std::fmt;

/// UE's `FGuid`: four `uint32`s, `A` to `D`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Guid(pub [u32; 4]);

impl Guid {
    pub const ZERO: Guid = Guid([0; 4]);

    /// A random GUID. Uniqueness on the bus is all that matters, not RFC 4122.
    pub fn new_random() -> Guid {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let mut words = [0u32; 4];
        for pair in words.chunks_mut(2) {
            let mut h = RandomState::new().build_hasher();
            h.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or_default(),
            );
            let v = h.finish();
            pair[0] = v as u32;
            pair[1] = (v >> 32) as u32;
        }
        Guid(words)
    }

    pub fn is_valid(&self) -> bool {
        *self != Guid::ZERO
    }

    /// Parses UE's default format, 32 hex digits (`A` to `D`, 8 each).
    pub fn parse(s: &str) -> Option<Guid> {
        let s: String = s.chars().filter(|c| *c != '-').collect();
        if s.len() != 32 {
            return None;
        }
        let mut words = [0u32; 4];
        for (i, w) in words.iter_mut().enumerate() {
            *w = u32::from_str_radix(&s[i * 8..i * 8 + 8], 16).ok()?;
        }
        Some(Guid(words))
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d] = self.0;
        write!(f, "{a:08X}{b:08X}{c:08X}{d:08X}")
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError(pub &'static str);

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for WireError {}

pub type WireResult<T> = Result<T, WireError>;

/// Writes values the way a saving `FMemoryArchive` does.
#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn guid(&mut self, g: Guid) -> &mut Self {
        for w in g.0 {
            self.u32(w);
        }
        self
    }
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(b);
        self
    }
    /// `FString`: length including the terminator, negative when stored as UTF-16.
    pub fn fstring(&mut self, s: &str) -> &mut Self {
        if s.is_empty() {
            return self.i32(0);
        }
        if s.is_ascii() {
            self.i32(s.len() as i32 + 1).bytes(s.as_bytes()).u8(0)
        } else {
            let units: Vec<u16> = s.encode_utf16().collect();
            self.i32(-(units.len() as i32 + 1));
            for u in units {
                self.u16(u);
            }
            self.u16(0)
        }
    }
}

/// Reads values the way a loading `FMemoryArchive` does.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }
    pub fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }
    pub fn take(&mut self, n: usize) -> WireResult<&'a [u8]> {
        if self.data.len() - self.pos < n {
            return Err(WireError("unexpected end of data"));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn array<const N: usize>(&mut self) -> WireResult<[u8; N]> {
        Ok(self.take(N)?.try_into().expect("length checked"))
    }
    pub fn u8(&mut self) -> WireResult<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> WireResult<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    pub fn u32(&mut self) -> WireResult<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    pub fn i32(&mut self) -> WireResult<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }
    pub fn u64(&mut self) -> WireResult<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    pub fn i64(&mut self) -> WireResult<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }
    pub fn guid(&mut self) -> WireResult<Guid> {
        Ok(Guid([self.u32()?, self.u32()?, self.u32()?, self.u32()?]))
    }
    /// A `TArray` count, checked against what is left so garbage can't allocate gigabytes.
    fn count(&mut self, elem_size: usize) -> WireResult<usize> {
        let n = self.i32()?;
        if n < 0 || (n as usize).saturating_mul(elem_size) > self.data.len() - self.pos {
            return Err(WireError("bad array length"));
        }
        Ok(n as usize)
    }
    pub fn byte_array(&mut self) -> WireResult<&'a [u8]> {
        let n = self.count(1)?;
        self.take(n)
    }
    pub fn fstring(&mut self) -> WireResult<String> {
        let n = self.i32()?;
        if n == 0 {
            return Ok(String::new());
        }
        if n > 0 {
            let raw = self.take(n as usize)?;
            let raw = raw.strip_suffix(&[0]).unwrap_or(raw);
            // ANSI here means Latin-1: each byte is one code point.
            Ok(raw.iter().map(|&b| b as char).collect())
        } else {
            let len = n.unsigned_abs() as usize;
            let raw = self.take(len.checked_mul(2).ok_or(WireError("bad string"))?)?;
            let (pairs, _) = raw.as_chunks::<2>();
            let mut units: Vec<u16> = pairs.iter().map(|c| u16::from_le_bytes(*c)).collect();
            if units.last() == Some(&0) {
                units.pop();
            }
            Ok(String::from_utf16_lossy(&units))
        }
    }
}

/// `EUdpMessageSegments`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SegmentType {
    None = 0,
    Abort,
    Acknowledge,
    Bye,
    Data,
    Hello,
    Retransmit,
    Timeout,
    AcknowledgeSegments,
    Ping,
    Pong,
    Mesh,
}

impl SegmentType {
    pub fn from_u8(v: u8) -> Option<SegmentType> {
        use SegmentType::*;
        Some(match v {
            0 => None,
            1 => Abort,
            2 => Acknowledge,
            3 => Bye,
            4 => Data,
            5 => Hello,
            6 => Retransmit,
            7 => Timeout,
            8 => AcknowledgeSegments,
            9 => Ping,
            10 => Pong,
            11 => Mesh,
            _ => return Option::None,
        })
    }
}

/// `FUdpMessageSegment::FHeader`, at the start of every datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub protocol_version: u8,
    /// Zero for multicast.
    pub recipient: Guid,
    pub sender: Guid,
    pub segment_type: u8,
}

impl Header {
    pub fn write(&self, w: &mut Writer) {
        w.u8(self.protocol_version)
            .guid(self.recipient)
            .guid(self.sender)
            .u8(self.segment_type);
    }
    pub fn read(r: &mut Reader) -> WireResult<Header> {
        Ok(Header {
            protocol_version: r.u8()?,
            recipient: r.guid()?,
            sender: r.guid()?,
            segment_type: r.u8()?,
        })
    }
}

/// `FDataChunk` in its protocol 12+ layout (we only speak 16+).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataChunk {
    pub message_id: i32,
    pub message_size: i64,
    pub message_flags: u32,
    pub segment_number: u32,
    pub segment_offset: u64,
    pub total_segments: u32,
    /// 0 means not sequential.
    pub sequence: u64,
    pub data: Vec<u8>,
}

impl DataChunk {
    /// Bytes of a data chunk besides its payload.
    pub const FIXED_BYTES: usize = 4 + 8 + 4 + 4 + 8 + 4 + 8 + 4;

    pub fn write(&self, w: &mut Writer) {
        w.i32(self.message_id)
            .i64(self.message_size)
            .u32(self.message_flags)
            .u32(self.segment_number)
            .u64(self.segment_offset)
            .u32(self.total_segments)
            .u64(self.sequence)
            .i32(self.data.len() as i32)
            .bytes(&self.data);
    }
    pub fn read(r: &mut Reader) -> WireResult<DataChunk> {
        Ok(DataChunk {
            message_id: r.i32()?,
            message_size: r.i64()?,
            message_flags: r.u32()?,
            segment_number: r.u32()?,
            segment_offset: r.u64()?,
            total_segments: r.u32()?,
            sequence: r.u64()?,
            data: r.byte_array()?.to_vec(),
        })
    }
}

/// `FAcknowledgeSegmentsChunk` (protocol 12+).
pub fn write_ack_segments(w: &mut Writer, message_id: i32, segments: &[u32]) {
    w.i32(message_id).i32(segments.len() as i32);
    for s in segments {
        w.u32(*s);
    }
}

pub fn read_ack_segments(r: &mut Reader) -> WireResult<(i32, Vec<u32>)> {
    let id = r.i32()?;
    let n = r.count(4)?;
    let segs = (0..n).map(|_| r.u32()).collect::<WireResult<_>>()?;
    Ok((id, segs))
}

/// `FRetransmitChunk`.
pub fn read_retransmit(r: &mut Reader) -> WireResult<(i32, Vec<u16>)> {
    let id = r.i32()?;
    let n = r.count(2)?;
    let segs = (0..n).map(|_| r.u16()).collect::<WireResult<_>>()?;
    Ok((id, segs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fstring_round_trips_ascii_unicode_and_empty() {
        for s in ["", "AutomationWorkerFindWorkers", "Ünïcødé ✓"] {
            let mut w = Writer::default();
            w.fstring(s);
            assert_eq!(Reader::new(&w.buf).fstring().unwrap(), s);
        }
        let mut w = Writer::default();
        w.fstring("ab");
        assert_eq!(w.buf, [3, 0, 0, 0, b'a', b'b', 0]);
    }

    #[test]
    fn header_and_data_chunk_round_trip() {
        let h = Header {
            protocol_version: 18,
            recipient: Guid::ZERO,
            sender: Guid([1, 2, 3, 4]),
            segment_type: SegmentType::Data as u8,
        };
        let c = DataChunk {
            message_id: 7,
            message_size: 3,
            message_flags: 0,
            segment_number: 0,
            segment_offset: 0,
            total_segments: 1,
            sequence: 0,
            data: vec![9, 8, 7],
        };
        let mut w = Writer::default();
        h.write(&mut w);
        c.write(&mut w);
        assert_eq!(w.buf.len(), 34 + DataChunk::FIXED_BYTES + 3);
        let mut r = Reader::new(&w.buf);
        assert_eq!(Header::read(&mut r).unwrap(), h);
        assert_eq!(DataChunk::read(&mut r).unwrap(), c);
    }

    #[test]
    fn guid_parses_ue_format() {
        let g = Guid([0x0123ABCD, 1, 2, 0xFFFFFFFF]);
        assert_eq!(Guid::parse(&g.to_string()), Some(g));
        assert_eq!(Guid::parse("nope"), None);
    }
}
