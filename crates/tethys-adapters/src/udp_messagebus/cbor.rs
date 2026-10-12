//! CBOR as UE's `FCborStructSerializerBackend` writes it: a struct is a map from property name to
//! value, enums are their names, containers are usually indefinite-length.
//!
//! UE's default wire format is "CBOR in platform endianness": standard CBOR, except that the
//! multi-byte arguments and floats after each initial byte are little-endian on x64. The decoder
//! takes the byte order as a parameter. The encoder always writes standard (big-endian) CBOR,
//! which the editor accepts when the message's format byte says so.

use super::wire::{Guid, WireError, WireResult};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// Integers, positive or negative. CBOR's range fits in i128.
    Int(i128),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Array(Vec<Value>),
    /// Keys are strings for everything UE writes (property names, exported map keys).
    Map(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn str(&self, key: &str) -> &str {
        match self.get(key) {
            Some(Value::Text(s)) => s,
            _ => "",
        }
    }
    pub fn int(&self, key: &str) -> i64 {
        match self.get(key) {
            Some(Value::Int(i)) => *i as i64,
            Some(Value::Float(f)) => *f as i64,
            _ => 0,
        }
    }
    pub fn float(&self, key: &str) -> f64 {
        match self.get(key) {
            Some(Value::Int(i)) => *i as f64,
            Some(Value::Float(f)) => *f,
            _ => 0.0,
        }
    }
    pub fn bool(&self, key: &str) -> bool {
        matches!(self.get(key), Some(Value::Bool(true)))
    }
    pub fn array(&self, key: &str) -> &[Value] {
        match self.get(key) {
            Some(Value::Array(a)) => a,
            _ => &[],
        }
    }
    /// An `FGuid` property, which the struct serializer writes as `{A, B, C, D}`.
    pub fn guid(&self, key: &str) -> Guid {
        match self.get(key) {
            Some(v @ Value::Map(_)) => Guid([
                v.int("A") as u32,
                v.int("B") as u32,
                v.int("C") as u32,
                v.int("D") as u32,
            ]),
            _ => Guid::ZERO,
        }
    }
}

/// Builds a struct value: `obj([("Name", "x".into()), ...])`.
pub fn obj<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Map(
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Text(s.to_string())
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Text(s)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}
impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i as i128)
    }
}
impl From<u32> for Value {
    fn from(i: u32) -> Self {
        Value::Int(i as i128)
    }
}
impl From<Guid> for Value {
    fn from(g: Guid) -> Self {
        let [a, b, c, d] = g.0;
        obj([
            ("A", a.into()),
            ("B", b.into()),
            ("C", c.into()),
            ("D", d.into()),
        ])
    }
}

const BREAK: u8 = 0xFF;

struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    little_endian: bool,
}

impl Decoder<'_> {
    fn take(&mut self, n: usize) -> WireResult<&[u8]> {
        if self.data.len() - self.pos < n {
            return Err(WireError("CBOR: unexpected end"));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn uint_bytes(&mut self, n: usize) -> WireResult<u64> {
        let little_endian = self.little_endian;
        let b = self.take(n)?;
        let mut v = 0u64;
        if little_endian {
            for &x in b.iter().rev() {
                v = (v << 8) | x as u64;
            }
        } else {
            for &x in b {
                v = (v << 8) | x as u64;
            }
        }
        Ok(v)
    }

    /// The argument of an initial byte; None for indefinite length.
    fn argument(&mut self, info: u8) -> WireResult<Option<u64>> {
        Ok(Some(match info {
            0..=23 => info as u64,
            24 => self.uint_bytes(1)?,
            25 => self.uint_bytes(2)?,
            26 => self.uint_bytes(4)?,
            27 => self.uint_bytes(8)?,
            31 => return Ok(None),
            _ => return Err(WireError("CBOR: reserved additional info")),
        }))
    }

    fn peek_break(&mut self) -> WireResult<bool> {
        match self.data.get(self.pos) {
            Some(&BREAK) => {
                self.pos += 1;
                Ok(true)
            }
            Some(_) => Ok(false),
            None => Err(WireError("CBOR: unexpected end")),
        }
    }

    fn len(&self, n: u64) -> WireResult<usize> {
        // Every item takes at least one byte, so a count above what is left is garbage.
        if n > (self.data.len() - self.pos) as u64 {
            return Err(WireError("CBOR: bad length"));
        }
        Ok(n as usize)
    }

    fn string_bytes(&mut self, major: u8, arg: Option<u64>) -> WireResult<Vec<u8>> {
        match arg {
            Some(n) => {
                let n = self.len(n)?;
                Ok(self.take(n)?.to_vec())
            }
            None => {
                let mut out = Vec::new();
                while !self.peek_break()? {
                    let ib = self.take(1)?[0];
                    if ib >> 5 != major {
                        return Err(WireError("CBOR: bad string chunk"));
                    }
                    let n = self
                        .argument(ib & 0x1F)?
                        .ok_or(WireError("CBOR: nested indefinite string"))?;
                    let n = self.len(n)?;
                    out.extend_from_slice(self.take(n)?);
                }
                Ok(out)
            }
        }
    }

    fn value(&mut self, depth: u32) -> WireResult<Value> {
        if depth > 64 {
            return Err(WireError("CBOR: nested too deeply"));
        }
        let ib = self.take(1)?[0];
        let (major, info) = (ib >> 5, ib & 0x1F);
        if major == 7 {
            return Ok(match info {
                20 => Value::Bool(false),
                21 => Value::Bool(true),
                22 | 23 => Value::Null,
                25 => Value::Float(half_to_f64(self.uint_bytes(2)? as u16)),
                26 => Value::Float(f32::from_bits(self.uint_bytes(4)? as u32) as f64),
                27 => Value::Float(f64::from_bits(self.uint_bytes(8)?)),
                0..=19 => Value::Int(info as i128),
                24 => Value::Int(self.uint_bytes(1)? as i128),
                _ => return Err(WireError("CBOR: unexpected simple value")),
            });
        }
        let arg = self.argument(info)?;
        Ok(match major {
            0 => Value::Int(arg.ok_or(WireError("CBOR: indefinite int"))? as i128),
            1 => Value::Int(-1 - arg.ok_or(WireError("CBOR: indefinite int"))? as i128),
            2 => Value::Bytes(self.string_bytes(2, arg)?),
            3 => Value::Text(String::from_utf8_lossy(&self.string_bytes(3, arg)?).into_owned()),
            4 => {
                let mut items = Vec::new();
                match arg {
                    Some(n) => {
                        for _ in 0..self.len(n)? {
                            items.push(self.value(depth + 1)?);
                        }
                    }
                    None => {
                        while !self.peek_break()? {
                            items.push(self.value(depth + 1)?);
                        }
                    }
                }
                Value::Array(items)
            }
            5 => {
                let mut entries = Vec::new();
                let mut entry = |d: &mut Self| -> WireResult<()> {
                    let key = match d.value(depth + 1)? {
                        Value::Text(s) => s,
                        Value::Int(i) => i.to_string(),
                        _ => return Err(WireError("CBOR: unsupported map key")),
                    };
                    entries.push((key, d.value(depth + 1)?));
                    Ok(())
                };
                match arg {
                    Some(n) => {
                        for _ in 0..self.len(n)? {
                            entry(self)?;
                        }
                    }
                    None => {
                        while !self.peek_break()? {
                            entry(self)?;
                        }
                    }
                }
                Value::Map(entries)
            }
            // A tag: ignore it and keep the tagged value.
            6 => self.value(depth + 1)?,
            _ => unreachable!("major type is three bits"),
        })
    }
}

fn half_to_f64(h: u16) -> f64 {
    let exp = (h >> 10) & 0x1F;
    let mant = (h & 0x3FF) as f64;
    let v = match exp {
        0 => mant * 2f64.powi(-24),
        31 if mant == 0.0 => f64::INFINITY,
        31 => f64::NAN,
        _ => (mant + 1024.0) * 2f64.powi(exp as i32 - 25),
    };
    if h & 0x8000 != 0 { -v } else { v }
}

/// Decodes one CBOR item from the start of `data`.
pub fn decode(data: &[u8], little_endian: bool) -> WireResult<Value> {
    Decoder {
        data,
        pos: 0,
        little_endian,
    }
    .value(0)
}

/// Encodes standard (big-endian) CBOR with definite lengths.
pub fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(&mut out, value);
    out
}

fn head(out: &mut Vec<u8>, major: u8, n: u64) {
    let m = major << 5;
    match n {
        0..=23 => out.push(m | n as u8),
        24..=0xFF => out.extend_from_slice(&[m | 24, n as u8]),
        0x100..=0xFFFF => {
            out.push(m | 25);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        0x1_0000..=0xFFFF_FFFF => {
            out.push(m | 26);
            out.extend_from_slice(&(n as u32).to_be_bytes());
        }
        _ => {
            out.push(m | 27);
            out.extend_from_slice(&n.to_be_bytes());
        }
    }
}

fn encode_into(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => out.push(0xF6),
        Value::Bool(b) => out.push(if *b { 0xF5 } else { 0xF4 }),
        Value::Int(i) if *i >= 0 => head(out, 0, *i as u64),
        Value::Int(i) => head(out, 1, (-1 - *i) as u64),
        Value::Float(f) => {
            out.push(0xFB);
            out.extend_from_slice(&f.to_be_bytes());
        }
        Value::Text(s) => {
            head(out, 3, s.len() as u64);
            out.extend_from_slice(s.as_bytes());
        }
        Value::Bytes(b) => {
            head(out, 2, b.len() as u64);
            out.extend_from_slice(b);
        }
        Value::Array(items) => {
            head(out, 4, items.len() as u64);
            for v in items {
                encode_into(out, v);
            }
        }
        Value::Map(entries) => {
            head(out, 5, entries.len() as u64);
            for (k, v) in entries {
                head(out, 3, k.len() as u64);
                out.extend_from_slice(k.as_bytes());
                encode_into(out, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_standard_encoding() {
        let v = obj([
            ("Name", "Subliminal.Inventory".into()),
            ("Count", 70000u32.into()),
            ("Neg", (-300i64).into()),
            ("Ok", true.into()),
            ("Duration", Value::Float(1.5)),
            ("Id", Guid([1, 0xFFFF_FFFF, 3, 4]).into()),
            ("Tests", Value::Array(vec![Value::Null, "x".into()])),
        ]);
        let back = decode(&encode(&v), false).unwrap();
        assert_eq!(back, v);
        assert_eq!(back.guid("Id"), Guid([1, 0xFFFF_FFFF, 3, 4]));
    }

    #[test]
    fn decodes_ue_platform_endianness_and_indefinite_containers() {
        // {_ "N": 300 (uint16 little-endian), "F": 1.0f32 (little-endian), "A": [_ 1] }
        let mut b = vec![0xBF];
        b.extend_from_slice(&[0x61, b'N', 0x19, 0x2C, 0x01]);
        b.extend_from_slice(&[0x61, b'F', 0xFA]);
        b.extend_from_slice(&1.0f32.to_le_bytes());
        b.extend_from_slice(&[0x61, b'A', 0x9F, 0x01, 0xFF]);
        b.push(0xFF);
        let v = decode(&b, true).unwrap();
        assert_eq!(v.int("N"), 300);
        assert_eq!(v.float("F"), 1.0);
        assert_eq!(v.array("A"), &[Value::Int(1)]);
    }

    #[test]
    fn rejects_lengths_longer_than_the_data() {
        assert!(decode(&[0x9B, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF], false).is_err());
    }
}
