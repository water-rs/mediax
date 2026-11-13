//! RBSP bit writing and emulation prevention for H.264 headers.

/// Writes an H.264 raw byte sequence payload one bit at a time, most
/// significant bit first.
#[derive(Debug, Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    /// Creates an empty writer.
    pub const fn new() -> Self {
        Self {
            bytes: Vec::new(),
            bits: 0,
        }
    }

    /// Returns the number of bits written so far.
    #[cfg(test)]
    pub const fn bit_len(&self) -> usize {
        self.bits
    }

    /// Writes one bit.
    pub fn put_bit(&mut self, bit: bool) {
        let used = self.bits % 8;
        if used == 0 {
            self.bytes.push(0);
        }
        if bit && let Some(last) = self.bytes.last_mut() {
            *last |= 0x80 >> used;
        }
        self.bits += 1;
    }

    /// Writes a one-bit flag, `u(1)`.
    pub fn put_flag(&mut self, flag: bool) {
        self.put_bit(flag);
    }

    /// Writes the low `count` bits of `value`, `u(count)`.
    pub fn put_bits(&mut self, value: u64, count: u32) {
        debug_assert!(count <= 64);
        for index in (0..count).rev() {
            self.put_bit((value >> index) & 1 == 1);
        }
    }

    /// Writes an unsigned Exp-Golomb code, `ue(v)`.
    pub fn put_ue(&mut self, value: u32) {
        self.put_exp_golomb(u64::from(value));
    }

    /// Writes a signed Exp-Golomb code, `se(v)`.
    pub fn put_se(&mut self, value: i32) {
        let value = i64::from(value);
        let code = if value > 0 { 2 * value - 1 } else { -2 * value };
        self.put_exp_golomb(code.unsigned_abs());
    }

    fn put_exp_golomb(&mut self, code_num: u64) {
        let shifted = code_num + 1;
        let length = shifted.ilog2() + 1;
        self.put_bits(0, length - 1);
        self.put_bits(shifted, length);
    }

    /// Appends `rbsp_trailing_bits()` and returns the byte-aligned payload.
    pub fn finish_rbsp(mut self) -> Vec<u8> {
        self.put_bit(true);
        while !self.bits.is_multiple_of(8) {
            self.put_bit(false);
        }
        self.bytes
    }

    /// Returns the bytes written, the last one zero padded, and the bit count.
    pub fn into_parts(self) -> (Vec<u8>, usize) {
        (self.bytes, self.bits)
    }
}

/// Inserts emulation prevention bytes (`0x03`) so that no `00 00 0x` with
/// `x <= 3` appears in a NAL unit payload (ITU-T H.264, 7.4.1).
pub fn escape_emulation(payload: &[u8]) -> Vec<u8> {
    let mut escaped = Vec::with_capacity(payload.len() + payload.len() / 32 + 1);
    let mut zeros = 0_u32;
    for &byte in payload {
        if zeros >= 2 && byte <= 3 {
            escaped.push(3);
            zeros = 0;
        }
        escaped.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(writer: BitWriter) -> String {
        let (bytes, count) = writer.into_parts();
        (0..count)
            .map(|index| {
                if bytes[index / 8] & (0x80 >> (index % 8)) == 0 {
                    '0'
                } else {
                    '1'
                }
            })
            .collect()
    }

    #[test]
    fn exp_golomb_codes_match_the_specification_table() {
        // ITU-T H.264 table 9-2.
        for (value, code) in [
            (0, "1"),
            (1, "010"),
            (2, "011"),
            (3, "00100"),
            (4, "00101"),
            (6, "00111"),
            (7, "0001000"),
            (14, "0001111"),
        ] {
            let mut writer = BitWriter::new();
            writer.put_ue(value);
            assert_eq!(bits(writer), code, "ue({value})");
        }
        // Table 9-3: se(v) maps 1, -1, 2, -2 to codeNum 1, 2, 3, 4.
        for (value, code) in [
            (0, "1"),
            (1, "010"),
            (-1, "011"),
            (2, "00100"),
            (-2, "00101"),
        ] {
            let mut writer = BitWriter::new();
            writer.put_se(value);
            assert_eq!(bits(writer), code, "se({value})");
        }
    }

    #[test]
    fn exp_golomb_handles_the_extremes() {
        let mut writer = BitWriter::new();
        writer.put_ue(u32::MAX);
        // codeNum + 1 = 2^32 needs 33 bits, so 32 leading zeros.
        assert_eq!(writer.bit_len(), 65);
        let mut writer = BitWriter::new();
        writer.put_se(i32::MIN);
        assert_eq!(writer.bit_len(), 65);
    }

    #[test]
    fn fixed_width_fields_are_msb_first() {
        let mut writer = BitWriter::new();
        writer.put_bits(0b101, 3);
        writer.put_flag(true);
        writer.put_bits(0x42, 8);
        assert_eq!(bits(writer), "101101000010");
    }

    #[test]
    fn trailing_bits_align_to_a_byte() {
        let mut writer = BitWriter::new();
        writer.put_bits(0b11, 2);
        assert_eq!(writer.finish_rbsp(), [0b1110_0000]);
        let mut writer = BitWriter::new();
        writer.put_bits(0xFF, 8);
        assert_eq!(writer.finish_rbsp(), [0xFF, 0x80]);
    }

    #[test]
    fn emulation_prevention_breaks_every_start_code_prefix() {
        assert_eq!(escape_emulation(&[0, 0, 0]), [0, 0, 3, 0]);
        assert_eq!(escape_emulation(&[0, 0, 1]), [0, 0, 3, 1]);
        assert_eq!(escape_emulation(&[0, 0, 2]), [0, 0, 3, 2]);
        assert_eq!(escape_emulation(&[0, 0, 3]), [0, 0, 3, 3]);
        assert_eq!(escape_emulation(&[0, 0, 4]), [0, 0, 4]);
        assert_eq!(escape_emulation(&[0, 1, 0, 0]), [0, 1, 0, 0]);
        // The inserted byte resets the zero run: 00 00 00 00 00 00 needs two.
        assert_eq!(
            escape_emulation(&[0, 0, 0, 0, 0, 0]),
            [0, 0, 3, 0, 0, 3, 0, 0]
        );
        assert_eq!(escape_emulation(&[]), Vec::<u8>::new());
    }
}
