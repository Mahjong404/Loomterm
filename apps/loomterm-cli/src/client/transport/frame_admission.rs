//! Count owned inline values and heap content without copying a decoded message.
//! The factor of two covers collection growth; struct/map padding is charged separately.
use serde::ser::{self, Serialize};
use std::fmt;

pub(super) const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct TooLarge;
impl fmt::Display for TooLarge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("decoded client frame exceeds the 64 MiB admission limit")
    }
}
impl std::error::Error for TooLarge {}
impl ser::Error for TooLarge {
    fn custom<T: fmt::Display>(_: T) -> Self {
        Self
    }
}

pub(super) fn decoded_bytes<T: Serialize>(value: &T) -> Result<usize, TooLarge> {
    let mut size = Footprint(0);
    size.visit(value)?;
    size.0
        .checked_mul(2)
        .filter(|bytes| *bytes <= MAX_FRAME_BYTES)
        .ok_or(TooLarge)
}

struct Footprint(usize);
impl Footprint {
    fn add(&mut self, bytes: usize) -> Result<(), TooLarge> {
        self.0 = self
            .0
            .checked_add(bytes)
            .filter(|total| *total <= MAX_FRAME_BYTES / 2)
            .ok_or(TooLarge)?;
        Ok(())
    }
    fn visit<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), TooLarge> {
        self.add(std::mem::size_of_val(value))?;
        value.serialize(self)
    }
}

macro_rules! scalar {
    ($($method:ident($ty:ty)),* $(,)?) => {$(
        fn $method(self, _: $ty) -> Result<(), TooLarge> { Ok(()) }
    )*};
}

impl ser::Serializer for &mut Footprint {
    type Ok = ();
    type Error = TooLarge;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;
    fn is_human_readable(&self) -> bool {
        false
    }
    scalar!(
        serialize_bool(bool),
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_i128(i128),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64),
        serialize_u128(u128),
        serialize_f32(f32),
        serialize_f64(f64),
        serialize_char(char)
    );
    fn serialize_str(self, value: &str) -> Result<(), TooLarge> {
        self.add(value.len())
    }
    fn serialize_bytes(self, value: &[u8]) -> Result<(), TooLarge> {
        self.add(value.len())
    }
    fn serialize_none(self) -> Result<(), TooLarge> {
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), TooLarge> {
        self.visit(value)
    }
    fn serialize_unit(self) -> Result<(), TooLarge> {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), TooLarge> {
        Ok(())
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
    ) -> Result<(), TooLarge> {
        Ok(())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<(), TooLarge> {
        self.visit(value)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        value: &T,
    ) -> Result<(), TooLarge> {
        self.visit(value)
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self, TooLarge> {
        self.add(24)?;
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self, TooLarge> {
        Ok(self)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, TooLarge> {
        Ok(self)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, TooLarge> {
        Ok(self)
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Self, TooLarge> {
        self.add(48usize.saturating_add(len.unwrap_or(0).saturating_mul(32)))?;
        Ok(self)
    }
    fn serialize_struct(self, _: &'static str, len: usize) -> Result<Self, TooLarge> {
        self.add(len.saturating_mul(8))?;
        Ok(self)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        len: usize,
    ) -> Result<Self, TooLarge> {
        self.add(len.saturating_mul(8))?;
        Ok(self)
    }
}

macro_rules! sequence {
    ($trait:ident, $method:ident) => {
        impl ser::$trait for &mut Footprint {
            type Ok = ();
            type Error = TooLarge;
            fn $method<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), TooLarge> {
                self.visit(value)
            }
            fn end(self) -> Result<(), TooLarge> {
                Ok(())
            }
        }
    };
}
sequence!(SerializeSeq, serialize_element);
sequence!(SerializeTuple, serialize_element);
sequence!(SerializeTupleStruct, serialize_field);
sequence!(SerializeTupleVariant, serialize_field);
impl ser::SerializeMap for &mut Footprint {
    type Ok = ();
    type Error = TooLarge;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), TooLarge> {
        self.visit(value)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), TooLarge> {
        self.visit(value)
    }
    fn end(self) -> Result<(), TooLarge> {
        Ok(())
    }
}
impl ser::SerializeStruct for &mut Footprint {
    type Ok = ();
    type Error = TooLarge;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        _: &'static str,
        value: &T,
    ) -> Result<(), TooLarge> {
        self.visit(value)
    }
    fn end(self) -> Result<(), TooLarge> {
        Ok(())
    }
}
impl ser::SerializeStructVariant for &mut Footprint {
    type Ok = ();
    type Error = TooLarge;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        _: &'static str,
        value: &T,
    ) -> Result<(), TooLarge> {
        self.visit(value)
    }
    fn end(self) -> Result<(), TooLarge> {
        Ok(())
    }
}
