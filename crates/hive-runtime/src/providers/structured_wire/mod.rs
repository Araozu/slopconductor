//! Bounded translation for structured gateway requests and responses.

mod common;
mod decoding;
mod encoding;
mod fold;
#[cfg(test)]
mod tests;

pub(super) use decoding::decode;
pub(super) use encoding::encode;
pub(super) use fold::StructuredFold;
