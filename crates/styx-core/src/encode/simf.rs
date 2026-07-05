//! `ToSimf`: Rust types that mirror SimplicityHL witness types.
//!
//! `ty()` and `value()` come from the same Rust type, so a value can never disagree with the
//! type it claims. For sums the sibling arm's type is supplied by the type parameter rather
//! than picked by hand at each call site; the prototype's `Value::right(hand_picked_ty, ...)`
//! pattern is where a wrong pick moves the payload into a different covenant arm.

use simplicityhl::types::{ResolvedType, TypeConstructible, UIntType};
use simplicityhl::value::ValueConstructible;
use simplicityhl::Value;

use crate::U256;

/// A SimplicityHL sum value, generic in both arm types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Either<A, B> {
    Left(A),
    Right(B),
}

/// A 64-byte Schnorr signature as a witness leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sig(pub [u8; 64]);

/// A Rust type with a fixed SimplicityHL witness type and a total encoding into it.
pub trait ToSimf {
    fn ty() -> ResolvedType;
    fn value(&self) -> Value;
}

impl ToSimf for () {
    fn ty() -> ResolvedType {
        ResolvedType::unit()
    }
    fn value(&self) -> Value {
        Value::unit()
    }
}

impl ToSimf for u32 {
    fn ty() -> ResolvedType {
        ResolvedType::from(UIntType::U32)
    }
    fn value(&self) -> Value {
        Value::u32(*self)
    }
}

impl ToSimf for u64 {
    fn ty() -> ResolvedType {
        ResolvedType::from(UIntType::U64)
    }
    fn value(&self) -> Value {
        Value::u64(*self)
    }
}

impl ToSimf for U256 {
    fn ty() -> ResolvedType {
        ResolvedType::from(UIntType::U256)
    }
    fn value(&self) -> Value {
        Value::u256(*self)
    }
}

impl ToSimf for Sig {
    fn ty() -> ResolvedType {
        ResolvedType::byte_array(64)
    }
    fn value(&self) -> Value {
        Value::byte_array(self.0)
    }
}

impl<A: ToSimf, B: ToSimf> ToSimf for (A, B) {
    fn ty() -> ResolvedType {
        ResolvedType::tuple([A::ty(), B::ty()])
    }
    fn value(&self) -> Value {
        Value::product(self.0.value(), self.1.value())
    }
}

impl<A: ToSimf, B: ToSimf> ToSimf for Either<A, B> {
    fn ty() -> ResolvedType {
        ResolvedType::either(A::ty(), B::ty())
    }
    fn value(&self) -> Value {
        match self {
            Either::Left(a) => Value::left(a.value(), B::ty()),
            Either::Right(b) => Value::right(A::ty(), b.value()),
        }
    }
}

impl<T: ToSimf, const N: usize> ToSimf for [T; N] {
    fn ty() -> ResolvedType {
        ResolvedType::array(T::ty(), N)
    }
    fn value(&self) -> Value {
        Value::array(self.iter().map(T::value), T::ty())
    }
}
