//! Jigsaw final states, read as command block states rather than lenient Anvil states.

use harpas_core::block::block_states;
use harpas_core::nbt::Nbt;

use super::ProcessorBlock;

pub(super) fn replace(mut block: ProcessorBlock) -> Option<ProcessorBlock> {
    let registry = block_states();
    if registry.properties(block.state)?.0 != "minecraft:jigsaw" {
        return Some(block);
    }
    let Some(tag) = &block.nbt else {
        return Some(block);
    };
    let text = match tag.get("final_state") {
        Some(Nbt::String(text)) => text.as_str(),
        _ => "minecraft:air",
    };
    block.state = parse_state(text)?;
    if block.state == registry.default_state("minecraft:structure_void")? {
        return None;
    }
    block.nbt = None;
    Some(block)
}

fn parse_state(mut text: &str) -> Option<u32> {
    let length = text
        .find(|c: char| !c.is_ascii_lowercase() && !c.is_ascii_digit() && !"_.:/-".contains(c))
        .unwrap_or(text.len());
    let id = &text[..length];
    text = &text[length..];
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    let namespace = if namespace.is_empty() {
        "minecraft"
    } else {
        namespace
    };
    let name = format!("{namespace}:{path}");
    let registry = block_states();
    let mut state = registry.default_state(&name)?;
    if take(&mut text, '[') {
        let mut properties = Vec::new();
        space(&mut text);
        while !take(&mut text, ']') {
            let key = string(&mut text)?;
            if properties.iter().any(|(previous, _)| previous == &key) {
                return None;
            }
            space(&mut text);
            if !take(&mut text, '=') {
                return None;
            }
            space(&mut text);
            let value = string(&mut text)?;
            properties.push((key, value));
            let (resolved, ignored) = registry.state(
                &name,
                properties
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            )?;
            if !ignored.is_empty() {
                return None;
            }
            state = resolved;
            space(&mut text);
            if take(&mut text, ']') {
                break;
            }
            if !take(&mut text, ',') {
                return None;
            }
            space(&mut text);
        }
    }
    // The command parser consumes a compound only immediately after the state. Other trailing
    // input, including whitespace before a compound or property list, is left unread.
    if text.starts_with('{') {
        super::snbt::compound(&mut text)?;
    }
    Some(state)
}

pub(super) fn take(text: &mut &str, token: char) -> bool {
    if let Some(rest) = text.strip_prefix(token) {
        *text = rest;
        true
    } else {
        false
    }
}

pub(super) fn space(text: &mut &str) {
    // Character.isWhitespace, which differs from Unicode White_Space for nonbreaking spaces.
    *text = text.trim_start_matches(|c| {
        matches!(c,
        '\u{9}'..='\u{d}' | '\u{1c}'..='\u{20}' | '\u{1680}' |
        '\u{2000}'..='\u{2006}' | '\u{2008}'..='\u{200a}' |
        '\u{2028}' | '\u{2029}' | '\u{205f}' | '\u{3000}')
    });
}

fn string(text: &mut &str) -> Option<String> {
    if let Some(quote @ ('\'' | '"')) = text.chars().next() {
        *text = &text[1..];
        let mut output = String::new();
        while let Some(c) = text.chars().next() {
            *text = &text[c.len_utf8()..];
            match c {
                c if c == quote => return Some(output),
                '\\' => {
                    let escaped = text.chars().next()?;
                    if escaped != quote && escaped != '\\' {
                        return None;
                    }
                    output.push(escaped);
                    *text = &text[escaped.len_utf8()..];
                }
                c => output.push(c),
            }
        }
        None
    } else {
        let length = text
            .find(|c: char| !c.is_ascii_alphanumeric() && !"_.+-".contains(c))
            .unwrap_or(text.len());
        let output = text[..length].to_owned();
        *text = &text[length..];
        Some(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_states_validate_properties_and_keep_unread_suffixes() {
        let registry = block_states();
        for text in [
            "oak_log[axis=x]",
            ":oak_log[ 'axis' = \"x\", ]",
            "oak_log[axis=x]ignored",
        ] {
            assert_eq!(
                parse_state(text),
                Some(
                    registry
                        .state("minecraft:oak_log", [("axis", "x")])
                        .unwrap()
                        .0
                ),
                "{text}"
            );
        }
        for text in ["oak_log", "oak_log[]", "oak_log [axis=x]"] {
            assert_eq!(
                parse_state(text),
                registry.default_state("minecraft:oak_log"),
                "{text}"
            );
        }
        for text in [
            "",
            " stone",
            "#minecraft:logs",
            "test:missing",
            "minecraft:stone:bad",
            "stone[axis=x]",
            "oak_log[axis=bad]",
            "oak_log[axis=x,axis=y]",
            "oak_log[axis=x",
            "oak_log[axis=\"x\\n\"]",
        ] {
            assert_eq!(parse_state(text), None, "{text}");
        }
    }

    #[test]
    fn jigsaws_preserve_missing_nbt_and_clear_it_only_on_replacement() {
        let registry = block_states();
        let block = ProcessorBlock {
            pos: [-17, 319, 32],
            state: registry.default_state("minecraft:jigsaw").unwrap(),
            nbt: None,
        };
        assert_eq!(replace(block.clone()), Some(block.clone()));
        for final_state in [None, Some(Nbt::Int(7)), Some(Nbt::String("air".into()))] {
            let mut tag = harpas_core::nbt::Compound::new();
            if let Some(value) = final_state {
                tag.insert("final_state", value);
            }
            let input = ProcessorBlock {
                nbt: Some(tag),
                ..block.clone()
            };
            assert_eq!(
                replace(input),
                Some(ProcessorBlock {
                    state: 0,
                    ..block.clone()
                })
            );
        }
        for text in ["structure_void", "stone[invalid=true]"] {
            let input = ProcessorBlock {
                nbt: Some(
                    [("final_state", Nbt::String(text.into()))]
                        .into_iter()
                        .collect(),
                ),
                ..block.clone()
            };
            assert_eq!(replace(input.clone()), None);
            let other = ProcessorBlock {
                state: registry.default_state("minecraft:structure_void").unwrap(),
                ..input
            };
            assert_eq!(replace(other.clone()), Some(other));
        }
    }
}
