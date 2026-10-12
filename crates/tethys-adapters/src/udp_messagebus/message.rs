//! A message as protocol 16–18 serializes it once its segments are reassembled
//! (`SerializeMessageV16_18` / `DeserializeV16_18` in UdpMessaging).

use super::cbor::{self, Value};
use super::wire::{Guid, Reader, WireError, WireResult, Writer};

/// `EMessageScope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Scope {
    Thread = 0,
    Process = 1,
    Network = 2,
    All = 3,
}

/// `EUdpMessageFormat`.
pub mod format {
    pub const JSON: u8 = 1;
    pub const CBOR_PLATFORM_ENDIANNESS: u8 = 3;
    pub const CBOR_STANDARD_ENDIANNESS: u8 = 4;
}

/// `FDateTime::MaxValue()`, which messages use as "never expires".
pub const DATETIME_MAX_TICKS: i64 = 3_155_378_975_999_999_999;

/// `FDateTime` ticks (100 ns since 0001-01-01) for now, in UTC.
pub fn now_ticks() -> i64 {
    const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
    let since_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    UNIX_EPOCH_TICKS + (since_unix.as_nanos() / 100) as i64
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// The struct's path, e.g. `/Script/AutomationMessages.AutomationWorkerFindWorkers`.
    pub type_path: String,
    /// The sending endpoint's `FMessageAddress`.
    pub sender: Guid,
    /// Empty for a published message.
    pub recipients: Vec<Guid>,
    pub scope: Scope,
    pub flags: u32,
    pub time_sent: i64,
    pub expiration: i64,
    pub annotations: Vec<(String, String)>,
    pub body: Value,
}

impl Message {
    pub fn new(type_path: &str, sender: Guid, recipients: Vec<Guid>, body: Value) -> Message {
        Message {
            type_path: type_path.to_string(),
            sender,
            recipients,
            scope: Scope::Network,
            flags: 0,
            time_sent: now_ticks(),
            expiration: DATETIME_MAX_TICKS,
            annotations: Vec::new(),
            body,
        }
    }

    /// The struct name without its package, e.g. `AutomationWorkerFindWorkers`.
    pub fn type_name(&self) -> &str {
        self.type_path.rsplit('.').next().unwrap_or(&self.type_path)
    }

    /// Serializes with the body in standard-endian CBOR.
    pub fn encode(&self) -> Vec<u8> {
        let (package, asset) = self
            .type_path
            .rsplit_once('.')
            .unwrap_or(("", &self.type_path));
        let mut w = Writer::default();
        w.fstring(package).fstring(asset).guid(self.sender);
        w.i32(self.recipients.len() as i32);
        for r in &self.recipients {
            w.guid(*r);
        }
        w.u8(self.scope as u8)
            .u32(self.flags)
            .i64(self.time_sent)
            .i64(self.expiration)
            .i32(self.annotations.len() as i32);
        for (k, v) in &self.annotations {
            w.fstring(k).fstring(v);
        }
        w.u8(format::CBOR_STANDARD_ENDIANNESS)
            .bytes(&cbor::encode(&self.body));
        w.buf
    }

    pub fn decode(data: &[u8]) -> WireResult<Message> {
        let mut r = Reader::new(data);
        let package = r.fstring()?;
        let asset = r.fstring()?;
        let sender = r.guid()?;
        let n = r.i32()?;
        if !(0..=1024).contains(&n) {
            return Err(WireError("bad recipient count"));
        }
        let recipients = (0..n).map(|_| r.guid()).collect::<WireResult<_>>()?;
        let scope = match r.u8()? {
            0 => Scope::Thread,
            1 => Scope::Process,
            2 => Scope::Network,
            3 => Scope::All,
            _ => return Err(WireError("bad scope")),
        };
        let flags = r.u32()?;
        let time_sent = r.i64()?;
        let expiration = r.i64()?;
        let n = r.i32()?;
        if !(0..=128).contains(&n) {
            return Err(WireError("bad annotation count"));
        }
        let annotations = (0..n)
            .map(|_| Ok((r.fstring()?, r.fstring()?)))
            .collect::<WireResult<_>>()?;
        let body = match r.u8()? {
            format::CBOR_PLATFORM_ENDIANNESS => {
                cbor::decode(r.rest(), cfg!(target_endian = "little"))?
            }
            format::CBOR_STANDARD_ENDIANNESS => cbor::decode(r.rest(), false)?,
            _ => return Err(WireError("unsupported message body format")),
        };
        Ok(Message {
            type_path: format!("{package}.{asset}"),
            sender,
            recipients,
            scope,
            flags,
            time_sent,
            expiration,
            annotations,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::udp_messagebus::cbor::obj;

    #[test]
    fn message_round_trips() {
        let mut m = Message::new(
            "/Script/AutomationMessages.AutomationWorkerFindWorkers",
            Guid([1, 2, 3, 4]),
            vec![Guid([5, 6, 7, 8])],
            obj([
                ("Changelist", 10000u32.into()),
                ("SessionId", Guid([9, 9, 9, 9]).into()),
            ]),
        );
        m.annotations.push(("Key".into(), "Value".into()));
        let back = Message::decode(&m.encode()).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.type_name(), "AutomationWorkerFindWorkers");
    }
}
