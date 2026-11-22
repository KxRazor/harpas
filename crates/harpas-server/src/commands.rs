//! The shared Brigadier tree for console input and Java Play commands.

use std::io;

use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::Handle;
use harpas_protocol_java::v26_1::play::{
    CommandNode, CommandNodeKind, CommandSuggestion, CommandTree,
};
use harpas_protocol_java::v26_1::play::{clientbound, serverbound};

use crate::session::format_uuid;

#[derive(Clone, Copy)]
pub enum Source {
    Console,
    Player(i32),
}

#[derive(Clone, Copy)]
enum Action {
    Help,
    List,
    ListUuids,
    Seed,
    Stop,
}

struct Node {
    literal: &'static str,
    /// `None` is console-only. Player permissions use the levels from `ops.json`.
    level: Option<i32>,
    allows_restricted: bool,
    action: Action,
    children: &'static [Node],
}

const ROOTS: &[Node] = &[
    Node {
        literal: "help",
        level: None,
        allows_restricted: false,
        action: Action::Help,
        children: &[],
    },
    Node {
        literal: "list",
        level: Some(0),
        allows_restricted: false,
        action: Action::List,
        children: &[Node {
            literal: "uuids",
            level: Some(0),
            allows_restricted: false,
            action: Action::ListUuids,
            children: &[],
        }],
    },
    Node {
        literal: "seed",
        level: Some(2),
        allows_restricted: true,
        action: Action::Seed,
        children: &[],
    },
    Node {
        literal: "stop",
        level: None,
        allows_restricted: false,
        action: Action::Stop,
        children: &[],
    },
];

impl Node {
    fn available(&self, source: Source) -> bool {
        match source {
            Source::Console => true,
            Source::Player(level) => self.level.is_some_and(|required| level >= required),
        }
    }
}

pub fn names() -> impl Iterator<Item = &'static str> {
    ROOTS.iter().map(|node| node.literal)
}

/// Projects the dispatcher's tree for one Java player's permission level.
pub fn tree(source: Source) -> clientbound::Commands {
    let root = CommandNode {
        kind: CommandNodeKind::Root,
        children: Vec::new(),
        redirect: None,
        executable: false,
        allows_restricted: false,
    };
    let mut nodes = vec![root];
    add_children(&mut nodes, 0, ROOTS, source);
    clientbound::Commands {
        tree: CommandTree { nodes, root: 0 },
    }
}

fn add_children(nodes: &mut Vec<CommandNode>, parent: usize, children: &[Node], source: Source) {
    for node in children.iter().filter(|node| node.available(source)) {
        let index = nodes.len();
        nodes[parent].children.push(index as i32);
        nodes.push(CommandNode {
            kind: CommandNodeKind::Literal(node.literal.into()),
            children: Vec::new(),
            redirect: None,
            executable: true,
            allows_restricted: node.allows_restricted,
        });
        add_children(nodes, index, node.children, source);
    }
}

/// Returns the byte index and literal completions for the final word.
pub fn complete(input: &str, source: Source) -> (usize, Vec<String>) {
    let mut start = usize::from(input.starts_with('/'));
    let mut remaining = &input[start..];
    let mut children = ROOTS;
    while let Some((word, tail)) = remaining.split_once(' ') {
        let Some(node) = find(children, word, source) else {
            return (start, Vec::new());
        };
        start += word.len() + 1;
        children = node.children;
        remaining = tail;
    }
    let prefix = remaining.to_ascii_lowercase();
    let matches = children
        .iter()
        .filter(|node| node.available(source) && node.literal.starts_with(&prefix))
        .map(|node| node.literal.into())
        .collect();
    (start, matches)
}

pub fn suggestions(
    request: serverbound::CommandSuggestionsRequest,
    source: Source,
) -> Option<clientbound::CommandSuggestionsResponse> {
    let (start, matches) = complete(&request.text, source);
    if matches.is_empty() {
        return None;
    }
    Some(clientbound::CommandSuggestionsResponse {
        id: request.id,
        start: request.text[..start].encode_utf16().count() as i32,
        length: request.text[start..].encode_utf16().count() as i32,
        suggestions: matches
            .into_iter()
            .map(|text| CommandSuggestion {
                text,
                tooltip: None,
            })
            .collect(),
    })
}

fn find<'a>(nodes: &'a [Node], literal: &str, source: Source) -> Option<&'a Node> {
    nodes
        .iter()
        .find(|node| node.literal == literal && node.available(source))
}

fn parse(input: &str, source: Source) -> Result<Action, (usize, &'static str)> {
    let mut start = 0;
    let mut remaining = input;
    let mut children = ROOTS;
    loop {
        let (word, tail) = remaining.split_once(' ').unwrap_or((remaining, ""));
        let Some(node) = find(children, word, source) else {
            return Err((
                start,
                if start == 0 {
                    "command.unknown.command"
                } else {
                    "command.unknown.argument"
                },
            ));
        };
        if word.len() == remaining.len() {
            return Ok(node.action);
        }
        if tail.is_empty() || node.children.is_empty() {
            return Err((start + word.len(), "command.unknown.argument"));
        }
        start += word.len() + 1;
        children = node.children;
        remaining = tail;
    }
}

#[derive(Clone, Copy)]
pub struct Commands {
    pub seed: i64,
    pub max_players: u32,
}

pub enum Outcome {
    Help,
    Stop,
    Feedback { text: String, component: Nbt },
    Syntax { cursor: usize, reason: &'static str },
}

impl Commands {
    pub fn run(self, logic: &Handle, input: &str, source: Source) -> io::Result<Outcome> {
        let action = match parse(input, source) {
            Ok(action) => action,
            Err((cursor, reason)) => return Ok(Outcome::Syntax { cursor, reason }),
        };
        Ok(match action {
            Action::Help => Outcome::Help,
            Action::Stop => Outcome::Stop,
            Action::Seed => {
                let seed = self.seed.to_string();
                let styled = Compound::from_iter([
                    ("text", Nbt::from(seed.as_str())),
                    ("color", "green".into()),
                    (
                        "click_event",
                        Compound::from_iter([
                            ("action", "copy_to_clipboard"),
                            ("value", seed.as_str()),
                        ])
                        .into(),
                    ),
                    (
                        "hover_event",
                        Compound::from_iter([
                            ("action", "show_text".into()),
                            ("value", translate("chat.copy.click", vec![])),
                        ])
                        .into(),
                    ),
                    ("insertion", seed.as_str().into()),
                ]);
                Outcome::Feedback {
                    text: format!("Seed: [{seed}]"),
                    component: translate(
                        "commands.seed.success",
                        vec![translate("chat.square_brackets", vec![styled.into()])],
                    ),
                }
            }
            Action::List | Action::ListUuids => {
                let players = logic.players().map_err(io::Error::other)?;
                let count = players.len() as i32;
                let with_uuids = matches!(action, Action::ListUuids);
                let names = players
                    .iter()
                    .map(|(uuid, name)| {
                        if with_uuids {
                            format!("{name} ({})", format_uuid(*uuid))
                        } else {
                            name.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let shown = players.into_iter().map(|(uuid, name)| {
                    if with_uuids {
                        Compound::from_iter([
                            ("translate", Nbt::from("commands.list.nameAndId")),
                            ("with", List::String(vec![name, format_uuid(uuid)]).into()),
                        ])
                    } else {
                        player_name(uuid, name)
                    }
                });
                Outcome::Feedback {
                    text: format!(
                        "There are {count} of a max of {} players online: {names}",
                        self.max_players
                    ),
                    component: translate(
                        "commands.list.players",
                        vec![
                            count.into(),
                            (self.max_players as i32).into(),
                            join_names(shown),
                        ],
                    ),
                }
            }
        })
    }

    pub fn player_reply(
        self,
        logic: &Handle,
        input: &str,
        source: Source,
    ) -> io::Result<Vec<clientbound::SystemChatMessage>> {
        let components = match self.run(logic, input, source)? {
            Outcome::Feedback { component, .. } => vec![component],
            Outcome::Syntax { cursor, reason } => vec![
                command_failure(Compound::from_iter([("translate", reason)])),
                command_failure(error_context(input, cursor)),
            ],
            Outcome::Help | Outcome::Stop => unreachable!("console-only commands were filtered"),
        };
        Ok(components
            .into_iter()
            .map(|content| clientbound::SystemChatMessage {
                content,
                overlay: false,
            })
            .collect())
    }
}

fn player_name(uuid: u128, name: String) -> Compound {
    let id = [96, 64, 32, 0].map(|shift| (uuid >> shift) as i32);
    Compound::from_iter([
        (
            "click_event",
            Nbt::from(suggest_command(format!("/tell {name} "))),
        ),
        ("insertion", name.as_str().into()),
        ("text", name.as_str().into()),
        (
            "hover_event",
            Compound::from_iter([
                ("action", "show_entity".into()),
                ("id", "minecraft:player".into()),
                ("uuid", Nbt::IntArray(id.to_vec())),
                ("name", name.into()),
            ])
            .into(),
        ),
    ])
}

fn join_names(mut names: impl Iterator<Item = Compound>) -> Nbt {
    let Some(first) = names.next() else {
        return "".into();
    };
    let Some(second) = names.next() else {
        return first.into();
    };
    let mut extra = vec![first];
    for name in std::iter::once(second).chain(names) {
        extra.push(Compound::from_iter([("text", ", "), ("color", "gray")]));
        extra.push(name);
    }
    Compound::from_iter([
        ("text", Nbt::from("")),
        ("extra", List::Compound(extra).into()),
    ])
    .into()
}

fn translate(key: &str, args: Vec<Nbt>) -> Nbt {
    let mut component = Compound::from_iter([("translate", key)]);
    if !args.is_empty() {
        component.insert(
            "with",
            List::Compound(
                args.into_iter()
                    .map(|arg| match arg {
                        Nbt::Compound(compound) => compound,
                        value => Compound::from_iter([("", value)]),
                    })
                    .collect(),
            ),
        );
    }
    component.into()
}

fn suggest_command(command: String) -> Compound {
    Compound::from_iter([("action", "suggest_command".into()), ("command", command)])
}

fn command_failure(message: Compound) -> Nbt {
    Compound::from_iter([
        ("text", Nbt::from("")),
        ("color", "red".into()),
        ("extra", List::Compound(vec![message]).into()),
    ])
    .into()
}

fn error_context(input: &str, cursor: usize) -> Compound {
    let mut extra = Vec::new();
    let before = &input[..cursor];
    let from = before
        .char_indices()
        .rev()
        .nth(9)
        .map_or(0, |(index, _)| index);
    if from != 0 {
        extra.push(Compound::from_iter([("", "...")]));
    }
    if from < cursor {
        extra.push(Compound::from_iter([("", &before[from..])]));
    }
    if cursor < input.len() {
        extra.push(Compound::from_iter([
            ("text", input[cursor..].into()),
            ("color", "red".into()),
            ("underlined", Nbt::Byte(1)),
        ]));
    }
    extra.push(Compound::from_iter([
        ("translate", "command.context.here".into()),
        ("color", "red".into()),
        ("italic", Nbt::Byte(1)),
    ]));
    Compound::from_iter([
        ("text", Nbt::from("")),
        ("color", "gray".into()),
        ("click_event", suggest_command(format!("/{input}")).into()),
        ("extra", List::Compound(extra).into()),
    ])
}
