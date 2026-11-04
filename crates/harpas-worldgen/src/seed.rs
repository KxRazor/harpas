//! Parsing the server's initial world seed with Java 25 string and integer semantics.

use harpas_core::math::java_string_hash;

// The starts of the ten-character BMP decimal digit ranges recognized by Java 25's Character.digit.
// Long.parseLong reads UTF-16 code units, so supplementary-plane digits are not accepted as digits.
const DECIMAL_ZEROES: &[u16] = &[
    0x0030, 0x0660, 0x06f0, 0x07c0, 0x0966, 0x09e6, 0x0a66, 0x0ae6, 0x0b66, 0x0be6, 0x0c66, 0x0ce6,
    0x0d66, 0x0de6, 0x0e50, 0x0ed0, 0x0f20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80,
    0x1a90, 0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
    0xff10,
];

/// Parses a `level-seed` value like Minecraft 26.1. Empty or ASCII-whitespace-only values return `None` so the
/// caller can choose a random seed. Decimal `i64` values are used directly; other text uses Java's UTF-16
/// `String.hashCode`, sign-extended to `i64`.
pub fn parse_level_seed(value: &str) -> Option<i64> {
    // Java's String.trim removes code points through U+0020, rather than all Unicode whitespace.
    let value = value.trim_matches(|c| c <= '\u{20}');
    if value.is_empty() {
        return None;
    }
    let units: Vec<u16> = value.encode_utf16().collect();
    let (negative, digits) = match units.first() {
        Some(0x2d) => (true, &units[1..]),
        Some(0x2b) => (false, &units[1..]),
        _ => (false, units.as_slice()),
    };
    let limit = if negative {
        (i64::MAX as u64) + 1
    } else {
        i64::MAX as u64
    };
    let parsed = (!digits.is_empty()).then(|| {
        digits.iter().try_fold(0_u64, |value, &unit| {
            let digit = DECIMAL_ZEROES.iter().find_map(|&zero| {
                unit.checked_sub(zero)
                    .filter(|&offset| offset < 10)
                    .map(u64::from)
            })?;
            let next = value.checked_mul(10)?.checked_add(digit)?;
            (next <= limit).then_some(next)
        })
    });
    match parsed.flatten() {
        Some(magnitude) if negative => Some((-(magnitude as i128)) as i64),
        Some(magnitude) => Some(magnitude as i64),
        None => Some(i64::from(java_string_hash(value)) + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_level_seed;

    #[test]
    fn parses_numeric_text_and_unicode_as_java_world_options_does() {
        let cases = [
            ("", None),
            (" \t\n", None),
            (" 42 ", Some(42)),
            ("0", Some(0)),
            ("-0", Some(0)),
            ("+42", Some(42)),
            ("-9223372036854775808", Some(i64::MIN)),
            ("9223372036854775807", Some(i64::MAX)),
            ("9223372036854775808", Some(-1_773_151_197)),
            ("-9223372036854775809", Some(1_304_595_159)),
            ("hello", Some(99_162_322)),
            ("😀", Some(1_772_899)),
            ("\u{2003}", Some(8_195)),
            ("١٢٣", Some(123)),
            ("-１２３", Some(-123)),
            ("𝟙𝟚𝟛", Some(-1_454_511_217)),
            ("+", Some(43)),
        ];
        for (input, expected) in cases {
            assert_eq!(parse_level_seed(input), expected, "{input:?}");
        }
    }
}
