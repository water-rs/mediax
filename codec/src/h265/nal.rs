//! NAL unit headers and types (7.3.1.2, table 7-1).

use super::error::H265Error;

/// The `nal_unit_type` of a NAL unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NalUnitType(pub u8);

impl NalUnitType {
    /// `BLA_W_LP`, the first of the broken link access types.
    pub const BLA_W_LP: Self = Self(16);
    /// `BLA_N_LP`, the last of the broken link access types.
    pub const BLA_N_LP: Self = Self(18);
    /// `IDR_W_RADL`.
    pub const IDR_W_RADL: Self = Self(19);
    /// `IDR_N_LP`.
    pub const IDR_N_LP: Self = Self(20);
    /// `RSV_IRAP_VCL23`, the last IRAP type.
    pub const LAST_IRAP: Self = Self(23);
    /// `VPS_NUT`.
    pub const VPS: u8 = 32;
    /// `SPS_NUT`.
    pub const SPS: u8 = 33;
    /// `PPS_NUT`.
    pub const PPS: u8 = 34;
    /// `EOS_NUT`.
    pub const EOS: u8 = 36;
    /// `EOB_NUT`.
    pub const EOB: u8 = 37;

    /// Whether the type is a coded slice segment this decoder decodes: the
    /// types 0 to 9 and 16 to 21. The other VCL types are reserved and
    /// ignored.
    #[must_use]
    pub const fn is_decodable_slice(self) -> bool {
        self.0 <= 9 || (self.0 >= 16 && self.0 <= 21)
    }

    /// Whether the picture is an intra random access point.
    #[must_use]
    pub const fn is_irap(self) -> bool {
        self.0 >= Self::BLA_W_LP.0 && self.0 <= Self::LAST_IRAP.0
    }

    /// Whether the picture is an IDR picture.
    #[must_use]
    pub const fn is_idr(self) -> bool {
        self.0 == Self::IDR_W_RADL.0 || self.0 == Self::IDR_N_LP.0
    }

    /// Whether the picture is a BLA picture.
    #[must_use]
    pub const fn is_bla(self) -> bool {
        self.0 >= Self::BLA_W_LP.0 && self.0 <= Self::BLA_N_LP.0
    }

    /// Whether the picture is a random access skipped leading picture.
    #[must_use]
    pub const fn is_rasl(self) -> bool {
        self.0 == 8 || self.0 == 9
    }

    /// Whether the picture is a random access decodable leading picture.
    #[must_use]
    pub const fn is_radl(self) -> bool {
        self.0 == 6 || self.0 == 7
    }

    /// Whether no picture of the same sub-layer refers to this one.
    #[must_use]
    pub const fn is_sub_layer_non_reference(self) -> bool {
        self.0 <= 14 && self.0.is_multiple_of(2)
    }
}

/// A NAL unit header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NalHeader {
    /// `nal_unit_type`.
    pub kind: u8,
    /// `nuh_layer_id`.
    pub layer_id: u8,
    /// `TemporalId`.
    pub temporal_id: u8,
}

impl NalHeader {
    /// The two bytes of the header.
    pub const LEN: usize = 2;

    /// Reads the header of `nal`.
    ///
    /// # Errors
    ///
    /// Returns [`H265Error::Invalid`] when the NAL unit is shorter than its
    /// header or the header's reserved bits are wrong.
    pub const fn parse(nal: &[u8]) -> Result<Self, H265Error> {
        let &[first, second, ..] = nal else {
            return Err(H265Error::Invalid("a NAL unit shorter than its header"));
        };
        if first & 0x80 != 0 {
            return Err(H265Error::Invalid("forbidden_zero_bit is set"));
        }
        let temporal_id_plus1 = second & 7;
        if temporal_id_plus1 == 0 {
            return Err(H265Error::Invalid("nuh_temporal_id_plus1 is 0"));
        }
        Ok(Self {
            kind: first >> 1,
            layer_id: (first & 1) << 5 | second >> 3,
            temporal_id: temporal_id_plus1 - 1,
        })
    }

    /// The type of a slice segment NAL unit.
    #[must_use]
    pub const fn unit_type(self) -> NalUnitType {
        NalUnitType(self.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_read_from_two_bytes() {
        // IDR_W_RADL (19), layer 0, TemporalId 0.
        let header = NalHeader::parse(&[0x26, 0x01]).unwrap();
        assert_eq!(
            header,
            NalHeader {
                kind: 19,
                layer_id: 0,
                temporal_id: 0
            }
        );
        // TRAIL_R (1), layer 2, TemporalId 3.
        let header = NalHeader::parse(&[0x02, 0x14]).unwrap();
        assert_eq!(
            header,
            NalHeader {
                kind: 1,
                layer_id: 2,
                temporal_id: 3
            }
        );
        // The layer id spans the two bytes.
        assert_eq!(NalHeader::parse(&[0x03, 0x09]).unwrap().layer_id, 33);
    }

    #[test]
    fn bad_headers_are_errors() {
        assert!(NalHeader::parse(&[0x26]).is_err());
        assert!(NalHeader::parse(&[0xA6, 0x01]).is_err());
        assert!(NalHeader::parse(&[0x26, 0x00]).is_err());
    }

    #[test]
    fn types_classify_like_table_7_1() {
        for kind in 0..=40_u8 {
            let unit = NalUnitType(kind);
            assert_eq!(unit.is_irap(), (16..=23).contains(&kind));
            assert_eq!(unit.is_idr(), matches!(kind, 19 | 20));
            assert_eq!(unit.is_bla(), (16..=18).contains(&kind));
            assert_eq!(unit.is_decodable_slice(), matches!(kind, 0..=9 | 16..=21));
        }
        assert!(NalUnitType(0).is_sub_layer_non_reference());
        assert!(NalUnitType(14).is_sub_layer_non_reference());
        assert!(!NalUnitType(1).is_sub_layer_non_reference());
        assert!(!NalUnitType(16).is_sub_layer_non_reference());
        assert!(NalUnitType(8).is_rasl() && NalUnitType(9).is_rasl());
        assert!(NalUnitType(6).is_radl() && NalUnitType(7).is_radl());
    }
}
