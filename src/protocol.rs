use std::fmt;

pub const PACKET_SIZE: usize = 12;
const HEADER: [u8; 5] = [0x55, 0xaa, 0x01, 0x01, 0x06];
const BLANK: [u8; 3] = [0xee; 3];
const MAX_TENTHS: u16 = 999;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Temperature(u16);

impl Temperature {
    pub fn from_millidegrees(value: i64) -> Option<Self> {
        if !(0..=99_949).contains(&value) {
            return None;
        }
        Some(Self(((value + 50) / 100) as u16))
    }

    pub fn from_degrees(value: u32) -> Option<Self> {
        (value <= u32::from(MAX_TENTHS / 10)).then(|| Self((value * 10) as u16))
    }

    fn digits(self) -> [u8; 3] {
        [
            (self.0 / 100) as u8,
            (self.0 / 10 % 10) as u8,
            (self.0 % 10) as u8,
        ]
    }
}

impl fmt::Display for Temperature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{} °C", self.0 / 10, self.0 % 10)
    }
}

pub fn packet(cpu: Option<Temperature>, gpu: Option<Temperature>) -> [u8; PACKET_SIZE] {
    let mut bytes = [0; PACKET_SIZE];
    bytes[..5].copy_from_slice(&HEADER);
    bytes[5..8].copy_from_slice(&cpu.map_or(BLANK, Temperature::digits));
    bytes[8..11].copy_from_slice(&gpu.map_or(BLANK, Temperature::digits));
    bytes[11] = bytes[..11].iter().copied().fold(0, u8::wrapping_add);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_fractional_values_in_correct_slots() {
        let cpu = Temperature::from_millidegrees(44_550);
        let gpu = Temperature::from_millidegrees(8_990);
        assert_eq!(
            packet(cpu, gpu),
            [0x55, 0xaa, 1, 1, 6, 4, 4, 6, 0, 9, 0, 30]
        );
        assert_eq!(cpu.unwrap().to_string(), "44.6 °C");
    }

    #[test]
    fn blanks_individual_fields_and_wraps_checksum() {
        let zero = Temperature::from_degrees(0);
        assert_eq!(
            packet(None, zero),
            [0x55, 0xaa, 1, 1, 6, 0xee, 0xee, 0xee, 0, 0, 0, 209]
        );
        assert_eq!(
            packet(zero, None),
            [0x55, 0xaa, 1, 1, 6, 0, 0, 0, 0xee, 0xee, 0xee, 209]
        );
        assert_eq!(
            packet(None, None),
            [0x55, 0xaa, 1, 1, 6, 0xee, 0xee, 0xee, 0xee, 0xee, 0xee, 155]
        );
    }

    #[test]
    fn rounds_in_integer_space_and_rejects_unrepresentable_values() {
        for (value, expected) in [
            (0, Some(0)),
            (49, Some(0)),
            (50, Some(1)),
            (9_950, Some(100)),
            (99_900, Some(999)),
            (99_949, Some(999)),
            (99_950, None),
            (-1, None),
            (100_000, None),
            (i64::MAX, None),
        ] {
            assert_eq!(Temperature::from_millidegrees(value).map(|t| t.0), expected);
        }
        assert_eq!(Temperature::from_degrees(99), Some(Temperature(990)));
        assert_eq!(Temperature::from_degrees(100), None);
        assert_eq!(Temperature::from_degrees(u32::MAX), None);
    }
}
