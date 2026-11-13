//! A reader of the syntax elements of a NAL unit's RBSP.

use super::{error::H265Error, nal::NalHeader};
use crate::rbsp::Counting;
use h264_reader::rbsp::{BitRead, BitReader, ByteReader};
use std::num::NonZeroUsize;

/// The bytes of the NAL unit header, which the reader skips.
const HEADER: NonZeroUsize = NonZeroUsize::new(NalHeader::LEN).unwrap();

/// Converts a value to the type its range check allows, failing rather than
/// substituting another value when the check and the type disagree.
///
/// # Errors
///
/// Returns [`H265Error::Invalid`] when `value` does not fit.
pub fn narrow<T: TryFrom<U>, U>(value: U) -> Result<T, H265Error> {
    T::try_from(value).map_err(|_| H265Error::Invalid("a value outside the range of its type"))
}

/// Reads fixed-length and Exp-Golomb coded elements after the NAL unit header,
/// without the emulation prevention bytes, and counts the bits read.
pub struct Syntax<'a> {
    reader: Counting<BitReader<ByteReader<&'a [u8]>>>,
    unit: &'static str,
}

impl<'a> Syntax<'a> {
    /// Starts reading the RBSP of `nal`, a `unit` for error messages.
    pub fn new(nal: &'a [u8], unit: &'static str) -> Self {
        Self {
            reader: Counting::new(BitReader::new(ByteReader::skipping_bytes(nal, HEADER))),
            unit,
        }
    }

    fn error(&self, error: &impl std::fmt::Debug) -> H265Error {
        H265Error::malformed(self.unit, error)
    }

    /// The number of bits read so far.
    pub const fn bits_read(&self) -> u64 {
        self.reader.bits()
    }

    /// Reads a one-bit flag.
    pub fn flag(&mut self, name: &'static str) -> Result<bool, H265Error> {
        self.reader.read_bit(name).map_err(|e| self.error(&e))
    }

    /// Reads `u(count)`, up to 32 bits.
    pub fn bits(&mut self, count: u32, name: &'static str) -> Result<u32, H265Error> {
        if count == 0 {
            return Ok(0);
        }
        self.reader
            .read_var::<u32>(count, name)
            .map_err(|e| self.error(&e))
    }

    /// Reads `ue(v)`.
    pub fn ue(&mut self, name: &'static str) -> Result<u32, H265Error> {
        self.reader.read_ue(name).map_err(|e| self.error(&e))
    }

    /// Reads `ue(v)` and checks it is at most `max`.
    pub fn ue_max(&mut self, max: u32, name: &'static str) -> Result<u32, H265Error> {
        let value = self.ue(name)?;
        if value > max {
            return Err(H265Error::Malformed {
                unit: self.unit,
                detail: format!("{name} {value} exceeds {max}"),
            });
        }
        Ok(value)
    }

    /// Reads `se(v)`.
    pub fn se(&mut self, name: &'static str) -> Result<i32, H265Error> {
        self.reader.read_se(name).map_err(|e| self.error(&e))
    }

    /// Reads `se(v)` and checks it lies in `min..=max`.
    pub fn se_range(&mut self, min: i32, max: i32, name: &'static str) -> Result<i32, H265Error> {
        let value = self.se(name)?;
        if !(min..=max).contains(&value) {
            return Err(H265Error::Malformed {
                unit: self.unit,
                detail: format!("{name} {value} is outside {min}..={max}"),
            });
        }
        Ok(value)
    }

    /// Skips `count` bits.
    pub fn skip(&mut self, count: u32, name: &'static str) -> Result<(), H265Error> {
        if count == 0 {
            return Ok(());
        }
        self.reader.skip(count, name).map_err(|e| self.error(&e))
    }

    /// Reads `byte_alignment()`: a one bit and zero bits up to the next byte.
    pub fn byte_alignment(&mut self) -> Result<(), H265Error> {
        if !self.flag("alignment_bit_equal_to_one")? {
            return Err(self.error(&"alignment_bit_equal_to_one is 0"));
        }
        let padding = (8 - self.bits_read() % 8) % 8;
        self.skip(narrow(padding)?, "alignment_bit_equal_to_zero")
    }
}

/// How many emulation prevention bytes precede the first `unescaped` bytes
/// of the RBSP in `nal`, and where in `nal` those bytes end.
#[must_use]
pub fn escaped_prefix(nal: &[u8], unescaped: usize) -> (usize, usize) {
    let mut zeros = 0;
    let mut kept = 0;
    let mut removed = 0;
    for (index, &byte) in nal.iter().enumerate() {
        if kept == unescaped {
            return (index, removed);
        }
        if zeros >= 2 && byte == 3 {
            removed += 1;
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        kept += 1;
    }
    (nal.len(), removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emulation_prevention_bytes_are_counted_up_to_a_length() {
        let nal = [0x40, 0x01, 0, 0, 3, 1, 0, 0, 3, 0, 7];
        // Unescaped: 40 01 00 00 01 00 00 00 07.
        assert_eq!(escaped_prefix(&nal, 2), (2, 0));
        assert_eq!(escaped_prefix(&nal, 4), (4, 0));
        assert_eq!(escaped_prefix(&nal, 5), (6, 1));
        assert_eq!(escaped_prefix(&nal, 8), (10, 2));
        assert_eq!(escaped_prefix(&nal, 9), (11, 2));
        assert_eq!(escaped_prefix(&nal, 100), (11, 2));
    }

    #[test]
    fn syntax_elements_read_after_the_header() {
        // Header, then 1 | 010 (ue 1) | 011 (ue 2) | 0 0 1 | 1 0 (alignment).
        let nal = [0x42, 0x01, 0b1010_0110, 0b0110_0000];
        let mut syntax = Syntax::new(&nal, "test");
        assert!(syntax.flag("a").unwrap());
        assert_eq!(syntax.ue("b").unwrap(), 1);
        assert_eq!(syntax.ue("c").unwrap(), 2);
        assert_eq!(syntax.bits(3, "d").unwrap(), 0b001);
        assert_eq!(syntax.bits_read(), 10);
        syntax.byte_alignment().unwrap();
        assert_eq!(syntax.bits_read(), 16);
        assert!(syntax.flag("past the end").is_err());
    }

    #[test]
    fn range_checks_are_errors_not_panics() {
        let nal = [0x42, 0x01, 0b0011_1000, 0];
        // ue 6, se -3.
        let mut syntax = Syntax::new(&nal, "test");
        assert!(syntax.ue_max(5, "x").is_err());
        let mut syntax = Syntax::new(&nal, "test");
        assert!(syntax.se_range(-1, 1, "y").is_err());
        let mut syntax = Syntax::new(&[0x42, 0x01, 0, 0], "test");
        assert!(syntax.ue("zeros").is_err());
    }
}
