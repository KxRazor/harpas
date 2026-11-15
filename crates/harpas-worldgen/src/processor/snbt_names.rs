//! Java 25's Character.codePointOf names: Unicode 16, with its control and range names.

// Java names UnicodeData's algorithmic ranges after their Unicode block, rather than using
// Unicode's algorithmic character names. Endpoints come from Unicode 16's UnicodeData.txt;
// the names and acceptance are checked against the JDK through the jigsaw parity probe.
const RANGES: &[(u32, u32, &str)] = &[
    (0x3400, 0x4dbf, "CJK UNIFIED IDEOGRAPHS EXTENSION A"),
    (0x4e00, 0x9fff, "CJK UNIFIED IDEOGRAPHS"),
    (0xac00, 0xd7a3, "HANGUL SYLLABLES"),
    (0xd800, 0xdb7f, "HIGH SURROGATES"),
    (0xdb80, 0xdbff, "HIGH PRIVATE USE SURROGATES"),
    (0xdc00, 0xdfff, "LOW SURROGATES"),
    (0xe000, 0xf8ff, "PRIVATE USE AREA"),
    (0x17000, 0x187f7, "TANGUT"),
    (0x18d00, 0x18d08, "TANGUT SUPPLEMENT"),
    (0x20000, 0x2a6df, "CJK UNIFIED IDEOGRAPHS EXTENSION B"),
    (0x2a700, 0x2b739, "CJK UNIFIED IDEOGRAPHS EXTENSION C"),
    (0x2b740, 0x2b81d, "CJK UNIFIED IDEOGRAPHS EXTENSION D"),
    (0x2b820, 0x2cea1, "CJK UNIFIED IDEOGRAPHS EXTENSION E"),
    (0x2ceb0, 0x2ebe0, "CJK UNIFIED IDEOGRAPHS EXTENSION F"),
    (0x2ebf0, 0x2ee5d, "CJK UNIFIED IDEOGRAPHS EXTENSION I"),
    (0x30000, 0x3134a, "CJK UNIFIED IDEOGRAPHS EXTENSION G"),
    (0x31350, 0x323af, "CJK UNIFIED IDEOGRAPHS EXTENSION H"),
    (0xf0000, 0xffffd, "SUPPLEMENTARY PRIVATE USE AREA A"),
    (0x100000, 0x10fffd, "SUPPLEMENTARY PRIVATE USE AREA B"),
];

const CONTROLS: &[&str] = &[
    "NULL",
    "START OF HEADING",
    "START OF TEXT",
    "END OF TEXT",
    "END OF TRANSMISSION",
    "ENQUIRY",
    "ACKNOWLEDGE",
    "BEL",
    "BACKSPACE",
    "CHARACTER TABULATION",
    "LINE FEED (LF)",
    "LINE TABULATION",
    "FORM FEED (FF)",
    "CARRIAGE RETURN (CR)",
    "SHIFT OUT",
    "SHIFT IN",
    "DATA LINK ESCAPE",
    "DEVICE CONTROL ONE",
    "DEVICE CONTROL TWO",
    "DEVICE CONTROL THREE",
    "DEVICE CONTROL FOUR",
    "NEGATIVE ACKNOWLEDGE",
    "SYNCHRONOUS IDLE",
    "END OF TRANSMISSION BLOCK",
    "CANCEL",
    "END OF MEDIUM",
    "SUBSTITUTE",
    "ESCAPE",
    "INFORMATION SEPARATOR FOUR",
    "INFORMATION SEPARATOR THREE",
    "INFORMATION SEPARATOR TWO",
    "INFORMATION SEPARATOR ONE",
    "DELETE",
    "PADDING CHARACTER",
    "HIGH OCTET PRESET",
    "BREAK PERMITTED HERE",
    "NO BREAK HERE",
    "LATIN 1 SUPPLEMENT 84",
    "NEXT LINE (NEL)",
    "START OF SELECTED AREA",
    "END OF SELECTED AREA",
    "CHARACTER TABULATION SET",
    "CHARACTER TABULATION WITH JUSTIFICATION",
    "LINE TABULATION SET",
    "PARTIAL LINE FORWARD",
    "PARTIAL LINE BACKWARD",
    "REVERSE LINE FEED",
    "SINGLE SHIFT TWO",
    "SINGLE SHIFT THREE",
    "DEVICE CONTROL STRING",
    "PRIVATE USE ONE",
    "PRIVATE USE TWO",
    "SET TRANSMIT STATE",
    "CANCEL CHARACTER",
    "MESSAGE WAITING",
    "START OF GUARDED AREA",
    "END OF GUARDED AREA",
    "START OF STRING",
    "SINGLE GRAPHIC CHARACTER INTRODUCER",
    "SINGLE CHARACTER INTRODUCER",
    "CONTROL SEQUENCE INTRODUCER",
    "STRING TERMINATOR",
    "OPERATING SYSTEM COMMAND",
    "PRIVACY MESSAGE",
    "APPLICATION PROGRAM COMMAND",
];

pub(super) fn codepoint(name: &str) -> Option<u32> {
    let name = name.trim_matches(|c| c <= '\u{20}').to_uppercase();
    if let Some(index) = CONTROLS.iter().position(|&control| control == name) {
        return Some(if index < 32 { index } else { index + 95 } as u32);
    }
    if let Some((block, hex)) = name.rsplit_once(' ')
        && let Ok(code) = u32::from_str_radix(hex, 16)
        && RANGES
            .iter()
            .any(|&(min, max, prefix)| (min..=max).contains(&code) && block == prefix)
        && name == format!("{block} {code:X}")
    {
        return Some(code);
    }
    let c = unicode_names2::character(&name)?;
    if RANGES
        .iter()
        .any(|&(min, max, _)| (min..=max).contains(&(c as u32)))
    {
        return None;
    }
    (unicode_names2::name(c)?.to_string() == name).then_some(c as u32)
}
