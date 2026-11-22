//! Validation of discarded command SNBT. A jigsaw keeps only the parsed block state, but vanilla
//! still rejects malformed attached compounds. Syntax is from Mojang's 1.21.5 SNBT release notes;
//! acceptance and numeric ranges are compared through the 26.1 replacement processor.

use super::jigsaw::{space, take};

enum Value {
    Number,
    String(Vec<u16>),
    Other,
}

pub(super) fn compound(text: &mut &str) -> Option<()> {
    map(text, 0)
}

fn map(text: &mut &str, depth: usize) -> Option<()> {
    if depth >= 512 || !take(text, '{') {
        return None;
    }
    space(text);
    if take(text, '}') {
        return Some(());
    }
    loop {
        string(text)?;
        space(text);
        if !take(text, ':') {
            return None;
        }
        value(text, depth + 1)?;
        if end_or_comma(text, '}')? {
            return Some(());
        }
    }
}

fn end_or_comma(text: &mut &str, end: char) -> Option<bool> {
    space(text);
    if take(text, end) {
        return Some(true);
    }
    if !take(text, ',') {
        return None;
    }
    space(text);
    Some(take(text, end))
}

fn value(text: &mut &str, depth: usize) -> Option<Value> {
    if depth >= 512 {
        return None;
    }
    space(text);
    match text.chars().next()? {
        '{' => {
            map(text, depth)?;
            Some(Value::Other)
        }
        '[' => {
            list(text, depth)?;
            Some(Value::Other)
        }
        '\'' | '"' => Some(Value::String(string(text)?)),
        c if c.is_ascii_digit() || ".+-".contains(c) => {
            number(text, None)?;
            Some(Value::Number)
        }
        _ => {
            let token = token(text);
            if token.is_empty() {
                return None;
            }
            space(text);
            if take(text, '(') {
                let argument = value(text, depth + 1)?;
                space(text);
                if take(text, ',') {
                    space(text);
                }
                if !take(text, ')') {
                    return None;
                }
                return match (token, argument) {
                    ("bool", Value::Number) => Some(Value::Number),
                    ("uuid", Value::String(string)) if uuid(&string) => Some(Value::Other),
                    _ => None,
                };
            }
            if token.eq_ignore_ascii_case("true") || token.eq_ignore_ascii_case("false") {
                Some(Value::Number)
            } else {
                Some(Value::String(token.encode_utf16().collect()))
            }
        }
    }
}

fn list(text: &mut &str, depth: usize) -> Option<()> {
    take(text, '[');
    space(text);
    let mut array = None;
    let mut probe = *text;
    if let Some(c @ ('B' | 'I' | 'L')) = probe.chars().next() {
        probe = &probe[1..];
        space(&mut probe);
        if take(&mut probe, ';') {
            array = Some(match c {
                'B' => 8,
                'I' => 32,
                _ => 64,
            });
            *text = probe;
            space(text);
        }
    }
    if take(text, ']') {
        return Some(());
    }
    loop {
        if let Some(bits) = array {
            if number(text, Some(bits))? > bits {
                return None;
            }
        } else {
            value(text, depth + 1)?;
        }
        if end_or_comma(text, ']')? {
            return Some(());
        }
    }
}

fn token<'a>(text: &mut &'a str) -> &'a str {
    let length = text
        .find(|c: char| !c.is_ascii_alphanumeric() && !"_.+-".contains(c))
        .unwrap_or(text.len());
    let output = &text[..length];
    *text = &text[length..];
    output
}

fn string(text: &mut &str) -> Option<Vec<u16>> {
    space(text);
    let Some(quote @ ('\'' | '"')) = text.chars().next() else {
        let word = token(text);
        return (!word.is_empty()).then(|| word.encode_utf16().collect());
    };
    *text = &text[1..];
    let mut output = Vec::new();
    loop {
        let c = text.chars().next()?;
        *text = &text[c.len_utf8()..];
        if c == quote {
            return Some(output);
        }
        let code = if c != '\\' {
            c as u32
        } else {
            let escape = text.chars().next()?;
            *text = &text[escape.len_utf8()..];
            match escape {
                '\\' | '\'' | '"' => escape as u32,
                'b' => 8,
                's' => 32,
                't' => 9,
                'n' => 10,
                'f' => 12,
                'r' => 13,
                'x' => hex(text, 2)?,
                'u' => hex(text, 4)?,
                'U' => hex(text, 8)?,
                'N' => {
                    if !take(text, '{') {
                        return None;
                    }
                    let end = text.find('}')?;
                    let name = &text[..end];
                    *text = &text[end + 1..];
                    if !name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || " -".contains(c))
                    {
                        return None;
                    }
                    super::snbt_names::codepoint(name)?
                }
                _ => return None,
            }
        };
        if code > 0x10ffff {
            return None;
        }
        if code <= 0xffff {
            // Java strings may contain isolated UTF-16 surrogates.
            output.push(code as u16);
        } else {
            let code = code - 0x10000;
            output.extend([0xd800 | (code >> 10) as u16, 0xdc00 | (code & 1023) as u16]);
        }
    }
}

fn hex(text: &mut &str, length: usize) -> Option<u32> {
    let digits = text.get(..length)?;
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    *text = &text[length..];
    Some(value)
}

fn uuid(string: &[u16]) -> bool {
    let Ok(string) = String::from_utf16(string) else {
        return false;
    };
    // UUID.fromString also accepts abbreviated groups; each is parsed as a positive signed long.
    string.len() <= 36
        && string.split('-').count() == 5
        && string.split('-').all(|group| {
            !group.is_empty()
                && i64::from_str_radix(group.strip_prefix('+').unwrap_or(group), 16).is_ok()
        })
}

fn digits(text: &mut &str, radix: u32) -> Option<String> {
    let length = text
        .find(|c: char| c != '_' && !c.is_digit(radix))
        .unwrap_or(text.len());
    let digits = &text[..length];
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') {
        return None;
    }
    *text = &text[length..];
    Some(digits.replace('_', ""))
}

fn number(text: &mut &str, array: Option<u32>) -> Option<u32> {
    space(text);
    let negative = take(text, '-');
    if !negative {
        take(text, '+');
    }
    space(text);
    let mut prefix = *text;
    let radix = if take(&mut prefix, '0') {
        space(&mut prefix);
        match prefix.chars().next() {
            Some('x' | 'X') => 16,
            Some('b' | 'B') => {
                let mut rest = &prefix[1..];
                space(&mut rest);
                if rest.starts_with(['0', '1']) { 2 } else { 10 }
            }
            _ => 10,
        }
    } else {
        10
    };
    if radix != 10 {
        *text = &prefix[1..];
        space(text);
    }
    let whole = if radix == 10 && text.starts_with('.') {
        String::new()
    } else {
        digits(text, radix)?
    };
    space(text);
    let mut float = String::new();
    if radix == 10 {
        float.push_str(&whole);
        if take(text, '.') {
            float.push('.');
            space(text);
            if text.starts_with(|c: char| c.is_ascii_digit()) {
                float.push_str(&digits(text, 10)?);
            } else if whole.is_empty() {
                return None;
            }
        }
        space(text);
        if text.starts_with(['e', 'E']) {
            *text = &text[1..];
            float.push('e');
            space(text);
            if text.starts_with(['+', '-']) {
                float.push(text.as_bytes()[0] as char);
                *text = &text[1..];
            }
            space(text);
            float.push_str(&digits(text, 10)?);
        }
        space(text);
        let fractional = float.contains(['.', 'e']);
        if fractional || text.starts_with(['f', 'F', 'd', 'D']) {
            if array.is_some() {
                return None;
            }
            let single = text.starts_with(['f', 'F']);
            if text.starts_with(['f', 'F', 'd', 'D']) {
                *text = &text[1..];
            }
            return if single {
                float.parse::<f32>().ok()?.is_finite().then_some(32)
            } else {
                float.parse::<f64>().ok()?.is_finite().then_some(64)
            };
        }
        if whole.len() > 1 && whole.starts_with('0') {
            return None;
        }
    }
    let mut signed = radix == 10;
    let mut suffix = text.chars().next().map(|c| c.to_ascii_lowercase());
    if matches!(suffix, Some('s' | 'u')) {
        let mut rest = &text[1..];
        space(&mut rest);
        if suffix == Some('u') || rest.starts_with(['b', 'B', 's', 'S', 'i', 'I', 'l', 'L']) {
            signed = suffix == Some('s');
            *text = rest;
            suffix = text.chars().next().map(|c| c.to_ascii_lowercase());
            if !matches!(suffix, Some('b' | 's' | 'i' | 'l')) {
                return None;
            }
        }
    }
    let bits = match suffix {
        Some('b') => 8,
        Some('s') => 16,
        Some('i') => 32,
        Some('l') => 64,
        _ => array.unwrap_or(32),
    };
    if matches!(suffix, Some('b' | 's' | 'i' | 'l')) {
        *text = &text[1..];
    }
    let magnitude = u128::from_str_radix(&whole, radix).ok()?;
    let limit = if signed {
        1u128 << (bits - 1)
    } else {
        1u128 << bits
    };
    if (negative && (!signed || magnitude > limit)) || (!negative && magnitude >= limit) {
        return None;
    }
    Some(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discarded_compounds_still_validate_numbers_lists_operations_and_strings() {
        for text in [
            r#"{}"#,
            r#"{x:[1,"a",{},],y:{z:true,},}"#,
            r#"{x:[B;127,-128,255ub],y:[I;1b,2s,3],z:[L;0xffffffffffffffff]}"#,
            r#"{x:bool(-.5f),y:uuid('1-1-1-1-1'),z:'\x42\uD800\U0001f600\N{Snowman}'}"#,
            r#"{x:1_2.3_4__5f,y:0b10_01,z:240ub}"#,
        ] {
            assert!(compound(&mut &*text).is_some(), "{text}");
        }
        for text in [
            "{",
            "{,}",
            "{a:}",
            "{a:1,,}",
            "{a:[,]}",
            "{a:[1,,]}",
            "{a:[I;1L]}",
            "{a:[B;128]}",
            "{a:01}",
            "{a:1e999f}",
            "{a:128b}",
            "{a:1_}",
            "{a:bool([])}",
            "{a:uuid('invalid')}",
            "{a:unknown(1)}",
            r#"{a:'\q'}"#,
            r#"{a:'\N{not a character}'}"#,
            r#"{a:'\N{DISTORTED FACE}'}"#, // Unicode 17, beyond Java 25's character data.
            r#"{a:'\U00110000'}"#,
        ] {
            assert!(compound(&mut &*text).is_none(), "{text}");
        }
    }
}
