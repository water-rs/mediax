//! Reading and writing the RBSP of NAL units, shared by the H.264 and H.265
//! parsers.

use h264_reader::rbsp::{BitRead, BitReaderError, Integer, Primitive};

/// Counts the bits a parser reads.
pub struct Counting<R> {
    inner: R,
    bits: u64,
}

impl<R> Counting<R> {
    /// Wraps `inner`, with nothing read yet.
    pub const fn new(inner: R) -> Self {
        Self { inner, bits: 0 }
    }

    /// The number of bits read so far.
    pub const fn bits(&self) -> u64 {
        self.bits
    }
}

impl<R: BitRead> BitRead for Counting<R> {
    fn read_ue(&mut self, name: &'static str) -> Result<u32, BitReaderError> {
        let value = self.inner.read_ue(name)?;
        // `value + 1` has `n + 1` bits and the code is `n` zeros then those.
        self.bits += u64::from(2 * (u64::from(value) + 1).ilog2() + 1);
        Ok(value)
    }

    fn read_se(&mut self, name: &'static str) -> Result<i32, BitReaderError> {
        let value = self.inner.read_se(name)?;
        let code = if value > 0 {
            i64::from(value) * 2 - 1
        } else {
            -i64::from(value) * 2
        };
        self.bits += u64::from(2 * (code.unsigned_abs() + 1).ilog2() + 1);
        Ok(value)
    }

    fn read_bit(&mut self, name: &'static str) -> Result<bool, BitReaderError> {
        let bit = self.inner.read_bit(name)?;
        self.bits += 1;
        Ok(bit)
    }

    fn read<const BITS: u32, I: Integer>(
        &mut self,
        name: &'static str,
    ) -> Result<I, BitReaderError> {
        let value = self.inner.read::<BITS, I>(name)?;
        self.bits += u64::from(BITS);
        Ok(value)
    }

    fn read_var<I: Integer>(
        &mut self,
        bit_count: u32,
        name: &'static str,
    ) -> Result<I, BitReaderError> {
        let value = self.inner.read_var::<I>(bit_count, name)?;
        self.bits += u64::from(bit_count);
        Ok(value)
    }

    fn read_to<V: Primitive>(&mut self, name: &'static str) -> Result<V, BitReaderError> {
        let value = self.inner.read_to::<V>(name)?;
        self.bits += 8 * u64::try_from(size_of::<V>()).unwrap_or(0);
        Ok(value)
    }

    fn skip(&mut self, bit_count: u32, name: &'static str) -> Result<(), BitReaderError> {
        self.inner.skip(bit_count, name)?;
        self.bits += u64::from(bit_count);
        Ok(())
    }

    fn byte_aligned(&self) -> bool {
        self.inner.byte_aligned()
    }

    fn has_more_rbsp_data(&mut self, name: &'static str) -> Result<bool, BitReaderError> {
        self.inner.has_more_rbsp_data(name)
    }

    fn finish_rbsp(self) -> Result<(), BitReaderError> {
        self.inner.finish_rbsp()
    }

    fn finish_sei_payload(self) -> Result<(), BitReaderError> {
        self.inner.finish_sei_payload()
    }
}

/// Builders for the bytes of NAL units, for the tests and the fuzz targets.
#[cfg(any(test, feature = "fuzzing"))]
pub mod testing {
    use crate::annexb::START_CODE;

    /// Appends syntax elements bit by bit.
    #[derive(Debug, Default, Clone)]
    pub struct Bits(Vec<bool>);

    impl Bits {
        /// The number of bits written.
        #[must_use]
        pub const fn len(&self) -> usize {
            self.0.len()
        }

        /// Writes a flag.
        pub fn bit(&mut self, value: bool) {
            self.0.push(value);
        }

        /// Writes `u(width)`.
        pub fn u(&mut self, width: u8, value: u32) {
            for shift in (0..width).rev() {
                self.0.push(value >> shift & 1 == 1);
            }
        }

        /// Writes `ue(v)`.
        pub fn ue(&mut self, value: u32) {
            let code = u64::from(value) + 1;
            let width = 64 - code.leading_zeros();
            for _ in 1..width {
                self.0.push(false);
            }
            for shift in (0..width).rev() {
                self.0.push(code >> shift & 1 == 1);
            }
        }

        /// Writes `se(v)`.
        pub fn se(&mut self, value: i32) {
            self.ue(if value > 0 {
                value.unsigned_abs() * 2 - 1
            } else {
                value.unsigned_abs() * 2
            });
        }

        /// Ends the bits with the RBSP trailing bits and packs them.
        #[must_use]
        pub fn into_rbsp(mut self) -> Vec<u8> {
            self.0.push(true);
            while !self.0.len().is_multiple_of(8) {
                self.0.push(false);
            }
            self.0
                .chunks(8)
                .map(|byte| byte.iter().fold(0, |acc, bit| acc << 1 | u8::from(*bit)))
                .collect()
        }
    }

    /// Inserts emulation prevention bytes into an RBSP and prepends the
    /// header.
    #[must_use]
    pub fn nal_unit(header: &[u8], rbsp: &[u8]) -> Vec<u8> {
        let mut nal = header.to_vec();
        let mut zeros = 0;
        for &byte in rbsp {
            if zeros >= 2 && byte <= 3 {
                nal.push(3);
                zeros = 0;
            }
            nal.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        nal
    }

    /// Frames NAL units with four-byte start codes.
    #[must_use]
    pub fn annex_b(nals: &[Vec<u8>]) -> Vec<u8> {
        let mut stream = Vec::new();
        for nal in nals {
            stream.extend_from_slice(&START_CODE);
            stream.extend_from_slice(nal);
        }
        stream
    }
}
