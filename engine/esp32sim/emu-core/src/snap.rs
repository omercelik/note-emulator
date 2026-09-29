//! Machine-state snapshot encoding (G7): a compact, non-self-describing binary serde format.
//!
//! Integers are little-endian at their own width, lengths and variant indices are `u64`/`u32`,
//! byte slices are written in one piece. Nothing is self-describing: the reader must be the same
//! type definitions as the writer, which the snapshot container enforces with a schema hash.
//! `restore_in_place` uses serde's `deserialize_in_place`. Note serde resets `#[serde(skip)]`
//! fields to their defaults even in place; types that must survive a restore untouched (host
//! sockets, board trait objects) implement serde through `keep_on_restore!`.

use serde::de::{self, DeserializeSeed, IntoDeserializer, SeqAccess, VariantAccess, Visitor};
use serde::ser::{self, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapError(pub String);

impl fmt::Display for SnapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}
impl std::error::Error for SnapError {}
impl ser::Error for SnapError {
    fn custom<T: fmt::Display>(msg: T) -> Self { SnapError(msg.to_string()) }
}
impl de::Error for SnapError {
    fn custom<T: fmt::Display>(msg: T) -> Self { SnapError(msg.to_string()) }
}

type Result<T> = std::result::Result<T, SnapError>;

pub fn to_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    let mut s = Writer { out: Vec::new() };
    value.serialize(&mut s)?;
    Ok(s.out)
}

pub fn from_bytes<'a, T: serde::Deserialize<'a>>(bytes: &'a [u8]) -> Result<T> {
    let mut d = Reader { input: bytes };
    let v = T::deserialize(&mut d)?;
    d.finish()?;
    Ok(v)
}

/// Overwrite `place` from `bytes`; skipped fields are left as they are.
pub fn restore_in_place<'a, T: serde::Deserialize<'a>>(bytes: &'a [u8], place: &mut T) -> Result<()> {
    let mut d = Reader { input: bytes };
    T::deserialize_in_place(&mut d, place)?;
    d.finish()
}

pub struct Writer {
    out: Vec<u8>,
}

impl Writer {
    fn len(&mut self, n: usize) { self.out.extend_from_slice(&(n as u64).to_le_bytes()); }
}

impl<'a> ser::Serializer for &'a mut Writer {
    type Ok = ();
    type Error = SnapError;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_bool(self, v: bool) -> Result<()> { self.out.push(v as u8); Ok(()) }
    fn serialize_i8(self, v: i8) -> Result<()> { self.out.extend_from_slice(&v.to_le_bytes()); Ok(()) }
    fn serialize_i16(self, v: i16) -> Result<()> { self.out.extend_from_slice(&v.to_le_bytes()); Ok(()) }
    fn serialize_i32(self, v: i32) -> Result<()> { self.out.extend_from_slice(&v.to_le_bytes()); Ok(()) }
    fn serialize_i64(self, v: i64) -> Result<()> { self.out.extend_from_slice(&v.to_le_bytes()); Ok(()) }
    fn serialize_i128(self, v: i128) -> Result<()> { self.out.extend_from_slice(&v.to_le_bytes()); Ok(()) }
    fn serialize_u8(self, v: u8) -> Result<()> { self.out.push(v); Ok(()) }
    fn serialize_u16(self, v: u16) -> Result<()> { self.out.extend_from_slice(&v.to_le_bytes()); Ok(()) }
    fn serialize_u32(self, v: u32) -> Result<()> { self.out.extend_from_slice(&v.to_le_bytes()); Ok(()) }
    fn serialize_u64(self, v: u64) -> Result<()> { self.out.extend_from_slice(&v.to_le_bytes()); Ok(()) }
    fn serialize_u128(self, v: u128) -> Result<()> { self.out.extend_from_slice(&v.to_le_bytes()); Ok(()) }
    fn serialize_f32(self, v: f32) -> Result<()> { self.out.extend_from_slice(&v.to_bits().to_le_bytes()); Ok(()) }
    fn serialize_f64(self, v: f64) -> Result<()> { self.out.extend_from_slice(&v.to_bits().to_le_bytes()); Ok(()) }
    fn serialize_char(self, v: char) -> Result<()> { self.serialize_u32(v as u32) }
    fn serialize_str(self, v: &str) -> Result<()> { self.serialize_bytes(v.as_bytes()) }
    fn serialize_bytes(self, v: &[u8]) -> Result<()> { self.len(v.len()); self.out.extend_from_slice(v); Ok(()) }
    fn serialize_none(self) -> Result<()> { self.out.push(0); Ok(()) }
    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<()> { self.out.push(1); v.serialize(self) }
    fn serialize_unit(self) -> Result<()> { Ok(()) }
    fn serialize_unit_struct(self, _: &'static str) -> Result<()> { Ok(()) }
    fn serialize_unit_variant(self, _: &'static str, idx: u32, _: &'static str) -> Result<()> { self.serialize_u32(idx) }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, v: &T) -> Result<()> { v.serialize(self) }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(self, _: &'static str, idx: u32, _: &'static str, v: &T) -> Result<()> {
        self.serialize_u32(idx)?;
        v.serialize(self)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Self> {
        self.len(len.ok_or_else(|| SnapError("sequence without a length".into()))?);
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self> { Ok(self) }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self> { Ok(self) }
    fn serialize_tuple_variant(self, _: &'static str, idx: u32, _: &'static str, _: usize) -> Result<Self> {
        self.serialize_u32(idx)?;
        Ok(self)
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Self> {
        self.len(len.ok_or_else(|| SnapError("map without a length".into()))?);
        Ok(self)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self> { Ok(self) }
    fn serialize_struct_variant(self, _: &'static str, idx: u32, _: &'static str, _: usize) -> Result<Self> {
        self.serialize_u32(idx)?;
        Ok(self)
    }
    fn is_human_readable(&self) -> bool { false }
}

macro_rules! compound {
    ($($tr:ident :: $m:ident),*) => {$(
        impl<'a> ser::$tr for &'a mut Writer {
            type Ok = ();
            type Error = SnapError;
            fn $m<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<()> { v.serialize(&mut **self) }
            fn end(self) -> Result<()> { Ok(()) }
        }
    )*};
}
compound!(SerializeSeq::serialize_element, SerializeTuple::serialize_element,
          SerializeTupleStruct::serialize_field, SerializeTupleVariant::serialize_field);

impl<'a> ser::SerializeMap for &'a mut Writer {
    type Ok = ();
    type Error = SnapError;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, k: &T) -> Result<()> { k.serialize(&mut **self) }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<()> { v.serialize(&mut **self) }
    fn end(self) -> Result<()> { Ok(()) }
}
impl<'a> ser::SerializeStruct for &'a mut Writer {
    type Ok = ();
    type Error = SnapError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, _: &'static str, v: &T) -> Result<()> { v.serialize(&mut **self) }
    fn end(self) -> Result<()> { Ok(()) }
}
impl<'a> ser::SerializeStructVariant for &'a mut Writer {
    type Ok = ();
    type Error = SnapError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, _: &'static str, v: &T) -> Result<()> { v.serialize(&mut **self) }
    fn end(self) -> Result<()> { Ok(()) }
}

pub struct Reader<'de> {
    input: &'de [u8],
}

impl<'de> Reader<'de> {
    fn take(&mut self, n: usize) -> Result<&'de [u8]> {
        if self.input.len() < n {
            return Err(SnapError(format!("truncated: need {n} bytes, {} left", self.input.len())));
        }
        let (head, rest) = self.input.split_at(n);
        self.input = rest;
        Ok(head)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> { Ok(self.take(N)?.try_into().unwrap()) }
    fn len(&mut self) -> Result<usize> {
        let n = u64::from_le_bytes(self.array()?);
        // A length can never exceed the bytes left (every element takes at least... zero bytes for
        // units, so bound by a generous cap instead of the remaining input).
        if n > (1 << 34) { return Err(SnapError(format!("implausible length {n}"))); }
        Ok(n as usize)
    }
    fn finish(&self) -> Result<()> {
        if self.input.is_empty() { Ok(()) } else { Err(SnapError(format!("{} trailing bytes", self.input.len()))) }
    }
}

impl<'de, 'a> de::Deserializer<'de> for &'a mut Reader<'de> {
    type Error = SnapError;

    fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value> { Err(SnapError("format is not self-describing".into())) }
    fn deserialize_bool<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
        match self.take(1)?[0] {
            0 => v.visit_bool(false),
            1 => v.visit_bool(true),
            b => Err(SnapError(format!("bad bool {b}"))),
        }
    }
    fn deserialize_i8<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { v.visit_i8(i8::from_le_bytes(self.array()?)) }
    fn deserialize_i16<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { v.visit_i16(i16::from_le_bytes(self.array()?)) }
    fn deserialize_i32<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { v.visit_i32(i32::from_le_bytes(self.array()?)) }
    fn deserialize_i64<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { v.visit_i64(i64::from_le_bytes(self.array()?)) }
    fn deserialize_i128<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { v.visit_i128(i128::from_le_bytes(self.array()?)) }
    fn deserialize_u8<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { v.visit_u8(self.take(1)?[0]) }
    fn deserialize_u16<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { v.visit_u16(u16::from_le_bytes(self.array()?)) }
    fn deserialize_u32<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { v.visit_u32(u32::from_le_bytes(self.array()?)) }
    fn deserialize_u64<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { v.visit_u64(u64::from_le_bytes(self.array()?)) }
    fn deserialize_u128<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { v.visit_u128(u128::from_le_bytes(self.array()?)) }
    fn deserialize_f32<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { v.visit_f32(f32::from_bits(u32::from_le_bytes(self.array()?))) }
    fn deserialize_f64<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { v.visit_f64(f64::from_bits(u64::from_le_bytes(self.array()?))) }
    fn deserialize_char<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
        let c = u32::from_le_bytes(self.array()?);
        v.visit_char(char::from_u32(c).ok_or_else(|| SnapError(format!("bad char {c}")))?)
    }
    fn deserialize_str<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
        let n = self.len()?;
        let bytes = self.take(n)?;
        v.visit_borrowed_str(std::str::from_utf8(bytes).map_err(|e| SnapError(e.to_string()))?)
    }
    fn deserialize_string<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { self.deserialize_str(v) }
    fn deserialize_bytes<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
        let n = self.len()?;
        v.visit_borrowed_bytes(self.take(n)?)
    }
    fn deserialize_byte_buf<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { self.deserialize_bytes(v) }
    fn deserialize_option<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
        match self.take(1)?[0] {
            0 => v.visit_none(),
            1 => v.visit_some(self),
            b => Err(SnapError(format!("bad option tag {b}"))),
        }
    }
    fn deserialize_unit<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { v.visit_unit() }
    fn deserialize_unit_struct<V: Visitor<'de>>(self, _: &'static str, v: V) -> Result<V::Value> { v.visit_unit() }
    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _: &'static str, v: V) -> Result<V::Value> { v.visit_newtype_struct(self) }
    fn deserialize_seq<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
        let n = self.len()?;
        v.visit_seq(Counted { de: self, left: n })
    }
    fn deserialize_tuple<V: Visitor<'de>>(self, n: usize, v: V) -> Result<V::Value> { v.visit_seq(Counted { de: self, left: n }) }
    fn deserialize_tuple_struct<V: Visitor<'de>>(self, _: &'static str, n: usize, v: V) -> Result<V::Value> {
        v.visit_seq(Counted { de: self, left: n })
    }
    fn deserialize_map<V: Visitor<'de>>(self, v: V) -> Result<V::Value> {
        let n = self.len()?;
        v.visit_map(Counted { de: self, left: n })
    }
    fn deserialize_struct<V: Visitor<'de>>(self, _: &'static str, fields: &'static [&'static str], v: V) -> Result<V::Value> {
        v.visit_seq(Counted { de: self, left: fields.len() })
    }
    fn deserialize_enum<V: Visitor<'de>>(self, _: &'static str, _: &'static [&'static str], v: V) -> Result<V::Value> {
        v.visit_enum(self)
    }
    fn deserialize_identifier<V: Visitor<'de>>(self, v: V) -> Result<V::Value> { self.deserialize_u32(v) }
    fn deserialize_ignored_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value> { Err(SnapError("cannot skip in a non-self-describing format".into())) }
    fn is_human_readable(&self) -> bool { false }
}

struct Counted<'a, 'de> {
    de: &'a mut Reader<'de>,
    left: usize,
}

impl<'de, 'a> SeqAccess<'de> for Counted<'a, 'de> {
    type Error = SnapError;
    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
        if self.left == 0 { return Ok(None); }
        self.left -= 1;
        seed.deserialize(&mut *self.de).map(Some)
    }
    fn size_hint(&self) -> Option<usize> { Some(self.left.min(1 << 20)) }
}

impl<'de, 'a> de::MapAccess<'de> for Counted<'a, 'de> {
    type Error = SnapError;
    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        if self.left == 0 { return Ok(None); }
        self.left -= 1;
        seed.deserialize(&mut *self.de).map(Some)
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value> { seed.deserialize(&mut *self.de) }
}

impl<'de, 'a> de::EnumAccess<'de> for &'a mut Reader<'de> {
    type Error = SnapError;
    type Variant = Self;
    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self)> {
        let idx = u32::from_le_bytes(self.array()?);
        let v = seed.deserialize(idx.into_deserializer())?;
        Ok((v, self))
    }
}

impl<'de, 'a> VariantAccess<'de> for &'a mut Reader<'de> {
    type Error = SnapError;
    fn unit_variant(self) -> Result<()> { Ok(()) }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> { seed.deserialize(self) }
    fn tuple_variant<V: Visitor<'de>>(self, n: usize, v: V) -> Result<V::Value> { v.visit_seq(Counted { de: self, left: n }) }
    fn struct_variant<V: Visitor<'de>>(self, fields: &'static [&'static str], v: V) -> Result<V::Value> {
        v.visit_seq(Counted { de: self, left: fields.len() })
    }
}

/// `Vec<u8>` in one piece rather than byte by byte (`#[serde(with = "emu_core::snap::bytes")]`).
pub mod bytes {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> { s.serialize_bytes(v) }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { f.write_str("bytes") }
            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> { Ok(v.to_vec()) }
            fn visit_borrowed_bytes<E: serde::de::Error>(self, v: &'de [u8]) -> Result<Vec<u8>, E> { Ok(v.to_vec()) }
        }
        d.deserialize_bytes(V)
    }

    /// In place: reuse the allocation (flash and PSRAM are megabytes).
    pub fn deserialize_in_place<'de, D: Deserializer<'de>>(d: D, place: &mut Vec<u8>) -> Result<(), D::Error> {
        let v: Vec<u8> = deserialize(d)?;
        place.clear();
        place.extend_from_slice(&v);
        Ok(())
    }
}

/// Arrays longer than serde's built-in 32 (`#[serde(with = "emu_core::snap::arr")]`).
pub mod arr {
    use serde::de::{Error, SeqAccess, Visitor};
    use serde::ser::SerializeTuple;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::marker::PhantomData;

    pub fn serialize<S: Serializer, T: Serialize, const N: usize>(v: &[T; N], s: S) -> Result<S::Ok, S::Error> {
        let mut t = s.serialize_tuple(N)?;
        for x in v {
            t.serialize_element(x)?;
        }
        t.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>, const N: usize>(d: D) -> Result<[T; N], D::Error> {
        struct V<T, const N: usize>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const N: usize> Visitor<'de> for V<T, N> {
            type Value = [T; N];
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { write!(f, "an array of {N}") }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<[T; N], A::Error> {
                let mut out = Vec::with_capacity(N);
                for i in 0..N {
                    out.push(seq.next_element()?.ok_or_else(|| A::Error::invalid_length(i, &self))?);
                }
                out.try_into().map_err(|_| A::Error::custom("array length"))
            }
        }
        d.deserialize_tuple(N, V::<T, N>(PhantomData))
    }
}

/// `[[T; M]; N]` with an inner array longer than 32 (`#[serde(with = "emu_core::snap::arr2")]`).
pub mod arr2 {
    use serde::de::{Error, SeqAccess, Visitor};
    use serde::ser::SerializeTuple;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::marker::PhantomData;

    struct Row<'a, T, const M: usize>(&'a [T; M]);
    impl<T: Serialize, const M: usize> Serialize for Row<'_, T, M> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> { super::arr::serialize(self.0, s) }
    }
    struct OwnedRow<T, const M: usize>([T; M]);
    impl<'de, T: Deserialize<'de>, const M: usize> Deserialize<'de> for OwnedRow<T, M> {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> { super::arr::deserialize(d).map(OwnedRow) }
    }

    pub fn serialize<S: Serializer, T: Serialize, const M: usize, const N: usize>(v: &[[T; M]; N], s: S) -> Result<S::Ok, S::Error> {
        let mut t = s.serialize_tuple(N)?;
        for row in v {
            t.serialize_element(&Row(row))?;
        }
        t.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>, const M: usize, const N: usize>(d: D) -> Result<[[T; M]; N], D::Error> {
        struct V<T, const M: usize, const N: usize>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const M: usize, const N: usize> Visitor<'de> for V<T, M, N> {
            type Value = [[T; M]; N];
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { write!(f, "{N} arrays of {M}") }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::with_capacity(N);
                for i in 0..N {
                    let row: OwnedRow<T, M> = seq.next_element()?.ok_or_else(|| A::Error::invalid_length(i, &self))?;
                    out.push(row.0);
                }
                out.try_into().map_err(|_| A::Error::custom("array length"))
            }
        }
        d.deserialize_tuple(N, V::<T, M, N>(PhantomData))
    }
}

/// State that belongs to the host process, not the machine (sockets, trait objects wired to the
/// board, caches): it writes nothing, and an in-place restore leaves the current value alone.
/// A fresh (not in-place) deserialize is refused, since there is nothing to build it from.
#[macro_export]
macro_rules! keep_on_restore {
    ($($ty:ty),* $(,)?) => {$(
        impl ::serde::Serialize for $ty {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> ::std::result::Result<S::Ok, S::Error> { s.serialize_unit() }
        }
        impl<'de> ::serde::Deserialize<'de> for $ty {
            fn deserialize<D: ::serde::Deserializer<'de>>(_: D) -> ::std::result::Result<Self, D::Error> {
                Err(<D::Error as ::serde::de::Error>::custom(concat!(stringify!($ty), " is host state; restore in place")))
            }
            fn deserialize_in_place<D: ::serde::Deserializer<'de>>(d: D, _place: &mut Self) -> ::std::result::Result<(), D::Error> {
                <() as ::serde::Deserialize>::deserialize(d)
            }
        }
    )*};
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, PartialEq, Default)]
    enum Mode { #[default] Idle, Busy(u32), Transfer { left: u16, dma: Option<u8> } }

    #[derive(Serialize, Deserialize, Debug, PartialEq, Default)]
    struct Dev {
        regs: [u32; 4],
        #[serde(with = "bytes")]
        ram: Vec<u8>,
        mode: Mode,
        queue: std::collections::VecDeque<(u64, bool)>,
        names: std::collections::HashMap<u32, String>,
        wide: u128,
        #[serde(skip)]
        cache: Vec<u32>,
    }

    #[test]
    fn round_trip_and_in_place_keeps_skipped_fields() {
        let mut a = Dev { regs: [1, 2, 3, 4], ram: vec![9; 1000], mode: Mode::Transfer { left: 7, dma: Some(2) }, wide: u128::MAX - 5, ..Default::default() };
        a.queue.push_back((5, true));
        a.names.insert(3, "x".into());
        a.cache = vec![42];
        let bytes = to_bytes(&a).unwrap();
        let b: Dev = from_bytes(&bytes).unwrap();
        assert_eq!((b.regs, &b.ram, &b.mode, &b.queue, &b.names, b.wide), (a.regs, &a.ram, &a.mode, &a.queue, &a.names, a.wide));
        assert!(b.cache.is_empty());
        let mut c = Dev { cache: vec![7, 7], mode: Mode::Busy(1), ..Default::default() };
        restore_in_place(&bytes, &mut c).unwrap();
        // serde resets `#[serde(skip)]` fields in place too; host state uses `keep_on_restore!`.
        assert_eq!((c.mode, c.cache, c.ram.len()), (Mode::Transfer { left: 7, dma: Some(2) }, vec![], 1000));
    }

    struct Host(u32);
    keep_on_restore!(Host);

    #[derive(Serialize, Deserialize)]
    struct WithHost {
        #[serde(with = "arr")]
        big: [u16; 40],
        host: Host,
        list: Vec<(u8, Host)>,
    }

    #[test]
    fn big_arrays_and_kept_host_state() {
        let a = WithHost { big: std::array::from_fn(|i| i as u16), host: Host(1), list: vec![(4, Host(1))] };
        let bytes = to_bytes(&a).unwrap();
        let mut b = WithHost { big: [0; 40], host: Host(9), list: vec![(0, Host(9))] };
        restore_in_place(&bytes, &mut b).unwrap();
        assert_eq!((b.big[39], b.host.0, b.list[0].0, b.list[0].1 .0), (39, 9, 4, 9));
        assert!(from_bytes::<WithHost>(&bytes).is_err(), "no fresh host state");
    }

    #[test]
    fn truncated_and_trailing_input_are_errors() {
        let bytes = to_bytes(&Dev::default()).unwrap();
        assert!(from_bytes::<Dev>(&bytes[..bytes.len() - 1]).is_err());
        let mut long = bytes.clone();
        long.push(0);
        assert!(from_bytes::<Dev>(&long).is_err());
    }
}
