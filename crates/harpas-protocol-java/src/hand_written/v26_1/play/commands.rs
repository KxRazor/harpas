use crate::codec::{self, Decode, DecodeError, Encode};

/// Indexed nodes of the Java Play Commands packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandTree {
    /// The nodes, including the root.
    pub nodes: Vec<CommandNode>,
    /// Index of the root node.
    pub root: i32,
}

/// A Brigadier node advertised to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandNode {
    /// The node's kind and its data.
    pub kind: CommandNodeKind,
    /// Indices of the node's children.
    pub children: Vec<i32>,
    /// Target node for a redirect, if any.
    pub redirect: Option<i32>,
    /// Whether a command can run at this node.
    pub executable: bool,
    /// The node's `allows_restricted` flag.
    pub allows_restricted: bool,
}

/// The three node types in the command tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandNodeKind {
    /// The unnamed root.
    Root,
    /// A literal command word.
    Literal(String),
    /// A parsed argument, with its parser-specific properties.
    Argument {
        /// The argument's display name.
        name: String,
        /// Numeric ID in the command argument type registry.
        parser: i32,
        /// The wire bytes for this parser's properties.
        properties: Vec<u8>,
        /// ID of a server-side custom suggestion provider.
        suggestions: Option<String>,
    },
}

impl Encode for CommandTree {
    fn encode(&self, buf: &mut Vec<u8>) {
        codec::write_array(buf, &self.nodes, |buf, node| node.encode(buf));
        codec::write_varint(buf, self.root);
    }
}

impl Decode for CommandTree {
    fn decode(input: &mut &[u8]) -> Result<Self, DecodeError> {
        let nodes = codec::read_array(input, i32::MAX as usize, CommandNode::decode)?;
        let root = codec::read_varint(input)?;
        if !matches!(nodes.get(root as usize), Some(node) if node.kind == CommandNodeKind::Root) {
            return Err(invalid("command tree root", root));
        }
        for node in &nodes {
            for index in node.children.iter().chain(node.redirect.iter()).copied() {
                if index as usize >= nodes.len() {
                    return Err(invalid("command node index", index));
                }
            }
        }
        Ok(Self { nodes, root })
    }
}

impl CommandNode {
    fn encode(&self, buf: &mut Vec<u8>) {
        let kind = match &self.kind {
            CommandNodeKind::Root => 0,
            CommandNodeKind::Literal(_) => 1,
            CommandNodeKind::Argument { suggestions, .. } => {
                2 | (u8::from(suggestions.is_some()) << 4)
            }
        };
        buf.push(
            kind | (u8::from(self.executable) << 2)
                | (u8::from(self.redirect.is_some()) << 3)
                | (u8::from(self.allows_restricted) << 5),
        );
        codec::write_array(buf, &self.children, |buf, index| {
            codec::write_varint(buf, *index)
        });
        if let Some(redirect) = self.redirect {
            codec::write_varint(buf, redirect);
        }
        match &self.kind {
            CommandNodeKind::Root => {}
            CommandNodeKind::Literal(name) => codec::write_string(buf, name),
            CommandNodeKind::Argument {
                name,
                parser,
                properties,
                suggestions,
            } => {
                codec::write_string(buf, name);
                codec::write_varint(buf, *parser);
                buf.extend_from_slice(properties);
                if let Some(suggestions) = suggestions {
                    codec::write_string(buf, suggestions);
                }
            }
        }
    }

    fn decode(input: &mut &[u8]) -> Result<Self, DecodeError> {
        let [flags] = codec::read_fixed(input)?;
        let children = codec::read_array(input, i32::MAX as usize, codec::read_varint)?;
        let redirect = (flags & 8 != 0)
            .then(|| codec::read_varint(input))
            .transpose()?;
        let kind = match flags & 3 {
            0 => CommandNodeKind::Root,
            1 => CommandNodeKind::Literal(codec::read_string(input, 32767)?),
            2 => {
                let name = codec::read_string(input, 32767)?;
                let parser = codec::read_varint(input)?;
                let properties = parser_properties(input, parser)?;
                let suggestions = (flags & 16 != 0)
                    .then(|| codec::read_string(input, 32767))
                    .transpose()?;
                CommandNodeKind::Argument {
                    name,
                    parser,
                    properties,
                    suggestions,
                }
            }
            kind => return Err(invalid("command node type", i32::from(kind))),
        };
        Ok(Self {
            kind,
            children,
            redirect,
            executable: flags & 4 != 0,
            allows_restricted: flags & 32 != 0,
        })
    }
}

fn parser_properties(input: &mut &[u8], parser: i32) -> Result<Vec<u8>, DecodeError> {
    let start = *input;
    match parser {
        // Numeric parsers have a flags byte and up to two fixed-width bounds.
        1..=4 => {
            let [flags] = codec::read_fixed(input)?;
            for _ in 0..(flags & 3).count_ones() {
                if parser == 1 || parser == 3 {
                    codec::read_fixed::<4>(input)?;
                } else {
                    codec::read_fixed::<8>(input)?;
                }
            }
        }
        5 => {
            codec::read_varint(input)?;
        } // brigadier:string mode
        6 | 31 => {
            codec::read_fixed::<1>(input)?;
        } // entity and score-holder flags
        43 => {
            codec::read_fixed::<4>(input)?;
        } // minimum time
        44..=48 => {
            codec::read_string(input, 32767)?;
        } // resource registry
        0 | 7..=30 | 32..=42 | 49..=56 => {}
        _ => return Err(invalid("command argument parser", parser)),
    }
    Ok(start[..start.len() - input.len()].to_vec())
}

fn invalid(name: &'static str, value: i32) -> DecodeError {
    DecodeError::UnknownEnumValue {
        name,
        value: i64::from(value),
    }
}
