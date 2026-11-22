//! The shared Brigadier tree for console input and Java Play commands.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, MutexGuard};

use harpas_core::nbt::{Compound, List, Nbt};
use harpas_engine::Handle;
use harpas_protocol_java::v26_1::play::{
    CommandNode, CommandNodeKind, CommandSuggestion, CommandTree,
};
use harpas_protocol_java::v26_1::play::{clientbound, serverbound};

use crate::access::{
    AccessLists, BANNED, BANNED_TEXT, DEFAULT_BAN_REASON, DEFAULT_BAN_REASON_KEY, IP_BANNED,
    IP_BANNED_TEXT, NOT_WHITELISTED, NOT_WHITELISTED_TEXT, Profile,
};
use crate::profiles::Profiles;
use crate::session::format_uuid;

#[derive(Clone, Copy)]
pub enum Source {
    Console,
    Player { level: i32, uuid: u128 },
}

#[derive(Clone, Copy)]
enum Action {
    Help,
    List,
    ListUuids,
    Seed,
    Stop,
    WhitelistList,
    SetWhitelist(bool),
    ChangeWhitelist(bool),
    ReloadWhitelist,
    Ban,
    Pardon,
    BanIp,
    PardonIp,
    BanList(BanList),
    ChangeOperator(bool),
}

#[derive(Clone, Copy)]
enum BanList {
    All,
    Ips,
    Players,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Word {
    Literal(&'static str),
    Profiles,
    Ip,
    Reason,
}

struct Node {
    word: Word,
    /// `None` is console-only. Player permissions use the levels from `ops.json`.
    level: Option<i32>,
    allows_restricted: bool,
    action: Option<Action>,
    children: &'static [Node],
}

const OP_COMMAND_LEVEL: i32 = 3;

const ROOTS: &[Node] = &[
    Node {
        word: Word::Literal("ban"),
        level: Some(3),
        allows_restricted: true,
        action: None,
        children: &[Node {
            word: Word::Profiles,
            level: Some(3),
            allows_restricted: false,
            action: Some(Action::Ban),
            children: &[Node {
                word: Word::Reason,
                level: Some(3),
                allows_restricted: false,
                action: Some(Action::Ban),
                children: &[],
            }],
        }],
    },
    Node {
        word: Word::Literal("ban-ip"),
        level: Some(3),
        allows_restricted: true,
        action: None,
        children: &[Node {
            word: Word::Ip,
            level: Some(3),
            allows_restricted: false,
            action: Some(Action::BanIp),
            children: &[Node {
                word: Word::Reason,
                level: Some(3),
                allows_restricted: false,
                action: Some(Action::BanIp),
                children: &[],
            }],
        }],
    },
    Node {
        word: Word::Literal("banlist"),
        level: Some(3),
        allows_restricted: true,
        action: Some(Action::BanList(BanList::All)),
        children: &[
            Node {
                word: Word::Literal("ips"),
                level: Some(3),
                allows_restricted: false,
                action: Some(Action::BanList(BanList::Ips)),
                children: &[],
            },
            Node {
                word: Word::Literal("players"),
                level: Some(3),
                allows_restricted: false,
                action: Some(Action::BanList(BanList::Players)),
                children: &[],
            },
        ],
    },
    Node {
        word: Word::Literal("deop"),
        level: Some(OP_COMMAND_LEVEL),
        allows_restricted: true,
        action: None,
        children: &[operator_target(false)],
    },
    Node {
        word: Word::Literal("help"),
        level: None,
        allows_restricted: false,
        action: Some(Action::Help),
        children: &[],
    },
    Node {
        word: Word::Literal("list"),
        level: Some(0),
        allows_restricted: false,
        action: Some(Action::List),
        children: &[Node {
            word: Word::Literal("uuids"),
            level: Some(0),
            allows_restricted: false,
            action: Some(Action::ListUuids),
            children: &[],
        }],
    },
    Node {
        word: Word::Literal("op"),
        level: Some(OP_COMMAND_LEVEL),
        allows_restricted: true,
        action: None,
        children: &[operator_target(true)],
    },
    Node {
        word: Word::Literal("pardon"),
        level: Some(3),
        allows_restricted: true,
        action: None,
        children: &[Node {
            word: Word::Profiles,
            level: Some(3),
            allows_restricted: false,
            action: Some(Action::Pardon),
            children: &[],
        }],
    },
    Node {
        word: Word::Literal("pardon-ip"),
        level: Some(3),
        allows_restricted: true,
        action: None,
        children: &[Node {
            word: Word::Ip,
            level: Some(3),
            allows_restricted: false,
            action: Some(Action::PardonIp),
            children: &[],
        }],
    },
    Node {
        word: Word::Literal("seed"),
        level: Some(2),
        allows_restricted: true,
        action: Some(Action::Seed),
        children: &[],
    },
    Node {
        word: Word::Literal("stop"),
        level: None,
        allows_restricted: false,
        action: Some(Action::Stop),
        children: &[],
    },
    Node {
        word: Word::Literal("whitelist"),
        level: Some(3),
        allows_restricted: true,
        action: None,
        children: &[
            whitelist_change("add", true),
            Node {
                word: Word::Literal("list"),
                level: Some(3),
                allows_restricted: false,
                action: Some(Action::WhitelistList),
                children: &[],
            },
            Node {
                word: Word::Literal("off"),
                level: Some(3),
                allows_restricted: false,
                action: Some(Action::SetWhitelist(false)),
                children: &[],
            },
            Node {
                word: Word::Literal("on"),
                level: Some(3),
                allows_restricted: false,
                action: Some(Action::SetWhitelist(true)),
                children: &[],
            },
            Node {
                word: Word::Literal("reload"),
                level: Some(3),
                allows_restricted: false,
                action: Some(Action::ReloadWhitelist),
                children: &[],
            },
            whitelist_change("remove", false),
        ],
    },
];

const fn operator_target(grant: bool) -> Node {
    Node {
        word: Word::Profiles,
        level: Some(OP_COMMAND_LEVEL),
        allows_restricted: false,
        action: Some(Action::ChangeOperator(grant)),
        children: &[],
    }
}

const fn whitelist_change(literal: &'static str, add: bool) -> Node {
    const fn command_child(add: bool) -> Node {
        Node {
            word: Word::Profiles,
            level: Some(3),
            allows_restricted: false,
            action: Some(Action::ChangeWhitelist(add)),
            children: &[],
        }
    }
    const ADD: &[Node] = &[command_child(true)];
    const REMOVE: &[Node] = &[command_child(false)];
    Node {
        word: Word::Literal(literal),
        level: Some(3),
        allows_restricted: false,
        action: None,
        children: if add { ADD } else { REMOVE },
    }
}

impl Node {
    fn available(&self, source: Source) -> bool {
        match source {
            Source::Console => true,
            Source::Player { level, .. } => self.level.is_some_and(|required| level >= required),
        }
    }
}

pub fn names() -> impl Iterator<Item = &'static str> {
    ROOTS.iter().filter_map(|node| match node.word {
        Word::Literal(name) => Some(name),
        Word::Profiles | Word::Ip | Word::Reason => None,
    })
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
            kind: match node.word {
                Word::Literal(name) => CommandNodeKind::Literal(name.into()),
                Word::Profiles => CommandNodeKind::Argument {
                    name: "targets".into(),
                    parser: 7, // minecraft:game_profile
                    properties: Vec::new(),
                    suggestions: (!matches!(node.action, Some(Action::Ban)))
                        .then(|| "minecraft:ask_server".into()),
                },
                Word::Ip => CommandNodeKind::Argument {
                    name: "target".into(),
                    parser: 5,           // brigadier:string
                    properties: vec![0], // single word
                    suggestions: matches!(node.action, Some(Action::PardonIp))
                        .then(|| "minecraft:ask_server".into()),
                },
                Word::Reason => CommandNodeKind::Argument {
                    name: "reason".into(),
                    parser: 20, // minecraft:message
                    properties: Vec::new(),
                    suggestions: None,
                },
            },
            children: Vec::new(),
            redirect: None,
            executable: node.action.is_some(),
            allows_restricted: node.allows_restricted,
        });
        add_children(nodes, index, node.children, source);
    }
}

fn completion_nodes(input: &str, source: Source) -> (usize, &'static [Node]) {
    let mut start = usize::from(input.starts_with('/'));
    let mut remaining = &input[start..];
    let mut children = ROOTS;
    while let Some((word, tail)) = remaining.split_once(' ') {
        let Some(node) = find(children, word, source) else {
            return (start, &[]);
        };
        start += word.len() + 1;
        children = node.children;
        remaining = tail;
    }
    (start, children)
}

fn find<'a>(nodes: &'a [Node], literal: &str, source: Source) -> Option<&'a Node> {
    nodes.iter().find(|node| {
        matches!(node.word, Word::Literal(name) if name == literal) && node.available(source)
    })
}

fn parse(
    input: &str,
    source: Source,
) -> Result<(Action, &str, Option<&str>), (usize, &'static str)> {
    let mut start = 0;
    let mut remaining = input;
    let mut children = ROOTS;
    loop {
        let (word, tail) = remaining.split_once(' ').unwrap_or((remaining, ""));
        if let Some(node) = children
            .iter()
            .find(|node| matches!(node.word, Word::Profiles | Word::Ip) && node.available(source))
            && !word.is_empty()
        {
            if node.word == Word::Ip
                && let Some(index) = word.find(|c: char| {
                    !c.is_ascii_alphanumeric() && !matches!(c, '_' | '-' | '.' | '+')
                })
            {
                return Err((start + index, "command.expected.separator"));
            }
            let reason = if word.len() == remaining.len() {
                None
            } else if !tail.is_empty() && node.children.iter().any(|node| node.word == Word::Reason)
            {
                Some(tail)
            } else {
                return Err((start + word.len(), "command.unknown.argument"));
            };
            return Ok((
                node.action.expect("a target argument executes"),
                word,
                reason,
            ));
        }
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
            return node
                .action
                .map(|action| (action, "", None))
                .ok_or((input.len(), "command.unknown.command"));
        }
        if tail.is_empty() || node.children.is_empty() {
            return Err((start + word.len(), "command.unknown.argument"));
        }
        start += word.len() + 1;
        children = node.children;
        remaining = tail;
    }
}

pub struct Commands {
    seed: i64,
    max_players: u32,
    access: Mutex<AccessLists>,
    peers: Mutex<HashMap<u128, Peer>>,
    profiles: Profiles,
    spawn: [f64; 3],
}

struct Peer {
    entity_id: i32,
    ip: IpAddr,
    permissions: Sender<i32>,
}

pub struct ConnectedPeer<'a> {
    commands: &'a Commands,
    uuid: u128,
    entity_id: i32,
    permissions: Receiver<i32>,
}

impl ConnectedPeer<'_> {
    pub fn uuid(&self) -> u128 {
        self.uuid
    }

    pub fn permission_updates(&self) -> impl Iterator<Item = i32> + '_ {
        self.permissions.try_iter()
    }
}

impl Drop for ConnectedPeer<'_> {
    fn drop(&mut self) {
        let mut peers = self.commands.peers();
        if peers
            .get(&self.uuid)
            .is_some_and(|peer| peer.entity_id == self.entity_id)
        {
            peers.remove(&self.uuid);
        }
    }
}

pub type Disconnect = (&'static str, &'static str);

pub enum Outcome {
    Help,
    Stop,
    Feedback {
        replies: Vec<(String, Nbt)>,
        disconnect: Option<Disconnect>,
    },
    Syntax {
        cursor: usize,
        reason: &'static str,
    },
}

impl Commands {
    pub fn new(
        seed: i64,
        max_players: u32,
        access: AccessLists,
        profiles: Profiles,
        spawn: [f64; 3],
    ) -> Self {
        Self {
            seed,
            max_players,
            access: Mutex::new(access),
            peers: Mutex::default(),
            profiles,
            spawn,
        }
    }

    pub fn access(&self) -> MutexGuard<'_, AccessLists> {
        self.access
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn record_profile(&self, uuid: u128, name: String) {
        self.profiles.record(Profile { uuid, name });
    }

    fn peers(&self) -> MutexGuard<'_, HashMap<u128, Peer>> {
        self.peers.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Registers the peer in the same access-list critical section as the engine join.
    pub fn track_peer(&self, uuid: u128, entity_id: i32, ip: IpAddr) -> ConnectedPeer<'_> {
        let (sender, permissions) = mpsc::channel();
        self.peers().insert(
            uuid,
            Peer {
                entity_id,
                ip: ip.to_canonical(),
                permissions: sender,
            },
        );
        ConnectedPeer {
            commands: self,
            uuid,
            entity_id,
            permissions,
        }
    }

    pub fn player_source(&self, uuid: u128) -> Source {
        Source::Player {
            uuid,
            level: self.access().operator_level(uuid),
        }
    }

    pub fn complete(&self, logic: &Handle, input: &str, source: Source) -> (usize, Vec<String>) {
        let (start, children) = completion_nodes(input, source);
        let prefix = input[start..].to_lowercase();
        let mut matches = Vec::new();
        for node in children.iter().filter(|node| node.available(source)) {
            match (node.word, node.action) {
                (Word::Literal(name), _) => matches.push(name.into()),
                (
                    Word::Profiles,
                    Some(
                        Action::ChangeWhitelist(true) | Action::Ban | Action::ChangeOperator(true),
                    ),
                ) => {
                    if let Ok(players) = logic.players() {
                        let access = self.access();
                        matches.extend(players.into_iter().filter_map(|(uuid, name)| {
                            let already_listed = match node.action {
                                Some(Action::Ban) => access.is_banned(uuid),
                                Some(Action::ChangeOperator(true)) => access.is_operator(uuid),
                                _ => access.is_whitelisted(uuid),
                            };
                            (!already_listed).then_some(name)
                        }));
                    }
                }
                (Word::Profiles, Some(Action::ChangeWhitelist(false))) => {
                    matches.extend(
                        self.access()
                            .whitelist_names()
                            .into_iter()
                            .map(str::to_owned),
                    );
                }
                (Word::Profiles, Some(Action::Pardon)) => {
                    matches.extend(self.access().banned_names().map(str::to_owned));
                }
                (Word::Profiles, Some(Action::ChangeOperator(false))) => {
                    matches.extend(self.access().operator_names().map(str::to_owned));
                }
                (Word::Ip, Some(Action::PardonIp)) => {
                    matches.extend(self.access().banned_ips().map(str::to_owned));
                }
                (Word::Profiles, _) => unreachable!("a profile argument executes"),
                (Word::Ip | Word::Reason, _) => {}
            }
        }
        matches.retain(|name| name.to_lowercase().starts_with(&prefix));
        matches.sort_by_cached_key(|name| name.to_lowercase());
        matches.dedup();
        (start, matches)
    }

    pub fn suggestions(
        &self,
        logic: &Handle,
        request: serverbound::CommandSuggestionsRequest,
        source: Source,
    ) -> Option<clientbound::CommandSuggestionsResponse> {
        let (start, matches) = self.complete(logic, &request.text, source);
        if matches.is_empty() {
            let command = request.text.strip_prefix('/').unwrap_or(&request.text);
            let (root, _) = command.split_once(' ')?;
            if matches!(root, "ban-ip" | "pardon-ip" | "banlist" | "op" | "deop")
                && find(ROOTS, root, source).is_some()
            {
                return Some(clientbound::CommandSuggestionsResponse {
                    id: request.id,
                    start: 0,
                    length: 0,
                    suggestions: Vec::new(),
                });
            }
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

    pub fn run(&self, logic: &Handle, input: &str, source: Source) -> io::Result<Outcome> {
        let (action, argument, reason) = match parse(input, source) {
            Ok(action) => action,
            Err((cursor, reason)) => return Ok(Outcome::Syntax { cursor, reason }),
        };
        Ok(match action {
            Action::Help => Outcome::Help,
            Action::Stop => Outcome::Stop,
            Action::WhitelistList => {
                let access = self.access();
                let names = access.whitelist_names();
                let count = names.len();
                if names.is_empty() {
                    feedback(
                        "There are no whitelisted players".into(),
                        translate("commands.whitelist.none", vec![]),
                    )
                } else {
                    let names = names.join(", ");
                    feedback(
                        format!("There are {count} whitelisted player(s): {names}"),
                        translate(
                            "commands.whitelist.list",
                            vec![(count as i32).into(), names.into()],
                        ),
                    )
                }
            }
            Action::SetWhitelist(enabled) => self.set_whitelist(logic, enabled, source)?,
            Action::ChangeWhitelist(add) => {
                self.change_whitelist(logic, source, argument, add, input.len() - argument.len())?
            }
            Action::ReloadWhitelist => self.reload_whitelist(logic, source)?,
            Action::ChangeOperator(grant) => {
                let cursor = input.len() - argument.len();
                let profiles = match self.resolve_targets(logic, source, argument, cursor)? {
                    Ok(profiles) => profiles,
                    Err(outcome) => return Ok(outcome),
                };
                self.change_operators(logic, source, profiles, grant)?
            }
            Action::BanList(list) => self.list_bans(list),
            Action::PardonIp => self.pardon_ip(argument),
            Action::BanIp => {
                let reason = match self.command_reason(logic, source, reason, input.len())? {
                    Ok(reason) => reason,
                    Err(outcome) => return Ok(outcome),
                };
                self.ban_ip(logic, source, argument, reason.as_deref())?
            }
            Action::Ban | Action::Pardon => {
                let cursor =
                    input.len() - argument.len() - reason.map_or(0, |reason| reason.len() + 1);
                let profiles = match self.resolve_targets(logic, source, argument, cursor)? {
                    Ok(profiles) => profiles,
                    Err(outcome) => return Ok(outcome),
                };
                let reason = match self.command_reason(logic, source, reason, input.len())? {
                    Ok(reason) => reason,
                    Err(outcome) => return Ok(outcome),
                };
                self.change_bans(
                    logic,
                    source,
                    profiles,
                    matches!(action, Action::Ban),
                    reason.as_deref(),
                )?
            }
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
                feedback(
                    format!("Seed: [{seed}]"),
                    translate(
                        "commands.seed.success",
                        vec![translate("chat.square_brackets", vec![styled.into()])],
                    ),
                )
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
                feedback(
                    format!(
                        "There are {count} of a max of {} players online: {names}",
                        self.max_players
                    ),
                    translate(
                        "commands.list.players",
                        vec![
                            count.into(),
                            (self.max_players as i32).into(),
                            join_names(shown),
                        ],
                    ),
                )
            }
        })
    }

    fn set_whitelist(&self, logic: &Handle, enabled: bool, source: Source) -> io::Result<Outcome> {
        let mut access = self.access();
        let (state, key, already) = if enabled {
            (
                "on",
                "commands.whitelist.enabled",
                "commands.whitelist.alreadyOn",
            )
        } else {
            (
                "off",
                "commands.whitelist.disabled",
                "commands.whitelist.alreadyOff",
            )
        };
        match access.set_whitelist(enabled) {
            Ok(true) => {}
            Ok(false) => {
                return Ok(failure(
                    format!("Whitelist is already turned {state}"),
                    already,
                ));
            }
            Err(error) => {
                log::warn!("Failed to save the whitelist setting: {error}");
                return Ok(failure(
                    "Could not save the whitelist setting".into(),
                    "command.failed",
                ));
            }
        }
        let disconnect = Self::enforce_whitelist(&access, logic, source)?;
        Ok(Outcome::Feedback {
            replies: vec![(
                format!("Whitelist is now turned {state}"),
                translate(key, vec![]),
            )],
            disconnect,
        })
    }

    fn change_whitelist(
        &self,
        logic: &Handle,
        source: Source,
        target: &str,
        add: bool,
        cursor: usize,
    ) -> io::Result<Outcome> {
        let profiles = match self.resolve_targets(logic, source, target, cursor)? {
            Ok(profiles) => profiles,
            Err(outcome) => return Ok(outcome),
        };
        let mut access = self.access();
        let changed = match access.change_whitelist(profiles, add) {
            Ok(changed) => changed,
            Err(error) => {
                log::warn!("Failed to save the whitelist: {error}");
                return Ok(failure(
                    "Could not save the whitelist".into(),
                    "command.failed",
                ));
            }
        };
        let (key, empty, empty_key) = if add {
            (
                "commands.whitelist.add.success",
                "Player is already whitelisted",
                "commands.whitelist.add.failed",
            )
        } else {
            (
                "commands.whitelist.remove.success",
                "Player is not whitelisted",
                "commands.whitelist.remove.failed",
            )
        };
        if changed.is_empty() {
            return Ok(failure(empty.into(), empty_key));
        }
        let replies = changed
            .into_iter()
            .map(|Profile { name, .. }| {
                let text = if add {
                    format!("Added {name} to the whitelist")
                } else {
                    format!("Removed {name} from the whitelist")
                };
                (text, translate_strings(key, vec![name]))
            })
            .collect();
        let disconnect = if add {
            None
        } else {
            Self::enforce_whitelist(&access, logic, source)?
        };
        Ok(Outcome::Feedback {
            replies,
            disconnect,
        })
    }

    fn change_bans(
        &self,
        logic: &Handle,
        source: Source,
        profiles: Vec<Profile>,
        ban: bool,
        reason: Option<&str>,
    ) -> io::Result<Outcome> {
        let author = match (ban, source) {
            (true, Source::Player { uuid, .. }) => logic
                .players()
                .map_err(io::Error::other)?
                .into_iter()
                .find_map(|(id, name)| (id == uuid).then_some(name))
                .ok_or_else(|| io::Error::other("ban command sender left"))?,
            _ => "Server".into(),
        };
        let mut access = self.access();
        let changed = match access.change_bans(profiles, ban.then_some((author.as_str(), reason))) {
            Ok(changed) => changed,
            Err(error) => {
                log::warn!("Failed to save the player ban list: {error}");
                return Ok(failure(
                    "Could not save the player ban list".into(),
                    "command.failed",
                ));
            }
        };
        if changed.is_empty() {
            return Ok(if ban {
                failure(
                    "Nothing changed. The player is already banned".into(),
                    "commands.ban.failed",
                )
            } else {
                failure(
                    "Nothing changed. The player isn't banned".into(),
                    "commands.pardon.failed",
                )
            });
        }
        let disconnect = if ban {
            let banned: HashSet<_> = changed.iter().map(|profile| profile.uuid).collect();
            let self_ban = matches!(source, Source::Player { uuid, .. } if banned.contains(&uuid));
            logic
                .disconnect_players(banned, BANNED, BANNED_TEXT)
                .map_err(io::Error::other)?;
            self_ban.then_some((BANNED, BANNED_TEXT))
        } else {
            None
        };
        let replies = changed
            .into_iter()
            .map(|Profile { name, .. }| {
                if ban {
                    (
                        format!("Banned {name}: {}", reason.unwrap_or(DEFAULT_BAN_REASON)),
                        translate_ban("commands.ban.success", vec![name], reason),
                    )
                } else {
                    (
                        format!("Unbanned {name}"),
                        translate_strings("commands.pardon.success", vec![name]),
                    )
                }
            })
            .collect();
        Ok(Outcome::Feedback {
            replies,
            disconnect,
        })
    }

    fn change_operators(
        &self,
        logic: &Handle,
        source: Source,
        profiles: Vec<Profile>,
        grant: bool,
    ) -> io::Result<Outcome> {
        let mut access = self.access();
        // A profile lookup can overlap with another command revoking the sender's permissions.
        if let Source::Player { uuid, .. } = source
            && access.operator_level(uuid) < OP_COMMAND_LEVEL
        {
            return Ok(Outcome::Syntax {
                cursor: 0,
                reason: "command.unknown.command",
            });
        }
        let changed = match access.change_operators(profiles, grant) {
            Ok(changed) => changed,
            Err(error) => {
                log::warn!("Failed to save the operator list: {error}");
                return Ok(failure(
                    "Could not save the operator list".into(),
                    "command.failed",
                ));
            }
        };
        if changed.is_empty() {
            return Ok(if grant {
                failure(
                    "Nothing changed. The player already is an operator".into(),
                    "commands.op.failed",
                )
            } else {
                failure(
                    "Nothing changed. The player is not an operator".into(),
                    "commands.deop.failed",
                )
            });
        }
        {
            let peers = self.peers();
            for profile in &changed {
                if let Some(peer) = peers.get(&profile.uuid) {
                    let _ = peer.permissions.send(access.operator_level(profile.uuid));
                }
            }
        }
        let disconnect = if grant {
            None
        } else {
            Self::enforce_whitelist(&access, logic, source)?
        };
        let (role, key) = if grant {
            ("a server operator", "commands.op.success")
        } else {
            ("no longer a server operator", "commands.deop.success")
        };
        let replies = changed
            .into_iter()
            .map(|Profile { name, .. }| {
                (
                    format!("Made {name} {role}"),
                    translate_strings(key, vec![name]),
                )
            })
            .collect();
        Ok(Outcome::Feedback {
            replies,
            disconnect,
        })
    }

    fn list_bans(&self, list: BanList) -> Outcome {
        let access = self.access();
        let entries = match list {
            BanList::All => access
                .player_ban_entries()
                .chain(access.ip_ban_entries())
                .collect::<Vec<_>>(),
            BanList::Ips => access.ip_ban_entries().collect(),
            BanList::Players => access.player_ban_entries().collect(),
        };
        if entries.is_empty() {
            return feedback(
                "There are no bans".into(),
                translate("commands.banlist.none", vec![]),
            );
        }
        let count = entries.len();
        let mut replies = vec![(
            format!("There are {count} ban(s):"),
            Compound::from_iter([
                ("translate", Nbt::from("commands.banlist.list")),
                ("with", List::Int(vec![count as i32]).into()),
            ])
            .into(),
        )];
        replies.extend(entries.into_iter().map(|(target, author, reason)| {
            (
                format!(
                    "{target} was banned by {author}: {}",
                    reason.unwrap_or(DEFAULT_BAN_REASON)
                ),
                translate_ban(
                    "commands.banlist.entry",
                    vec![target.into(), author.into()],
                    reason,
                ),
            )
        }));
        Outcome::Feedback {
            replies,
            disconnect: None,
        }
    }

    fn ban_ip(
        &self,
        logic: &Handle,
        source: Source,
        target: &str,
        reason: Option<&str>,
    ) -> io::Result<Outcome> {
        let mut access = self.access();
        let players = logic.players().map_err(io::Error::other)?;
        let (ip, affected) = {
            let peers = self.peers();
            let player_ip = |uuid: &u128| peers.get(uuid).map(|peer| peer.ip);
            let ip = target
                .parse::<Ipv4Addr>()
                .map(IpAddr::from)
                .ok()
                .or_else(|| {
                    players
                        .iter()
                        .find(|(_, name)| name.eq_ignore_ascii_case(target))
                        .and_then(|(uuid, _)| player_ip(uuid))
                });
            let Some(ip) = ip else {
                return Ok(failure(
                    "Invalid IP address or unknown player".into(),
                    "commands.banip.invalid",
                ));
            };
            let affected = players
                .iter()
                .filter(|(uuid, _)| player_ip(uuid) == Some(ip))
                .collect::<Vec<_>>();
            (ip, affected)
        };
        let author = match source {
            Source::Console => "Server",
            Source::Player { uuid, .. } => players
                .iter()
                .find(|(id, _)| *id == uuid)
                .map(|(_, name)| name.as_str())
                .ok_or_else(|| io::Error::other("ban-ip command sender left"))?,
        };
        let ip = ip.to_string();
        match access.change_ip_ban(&ip, Some((author, reason))) {
            Ok(true) => {}
            Ok(false) => {
                return Ok(failure(
                    "Nothing changed. That IP is already banned".into(),
                    "commands.banip.failed",
                ));
            }
            Err(error) => return Ok(ip_ban_write_failed(error)),
        }
        let mut replies = vec![(
            format!("Banned IP {ip}: {}", reason.unwrap_or(DEFAULT_BAN_REASON)),
            translate_ban("commands.banip.success", vec![ip], reason),
        )];
        let mut disconnect = None;
        if !affected.is_empty() {
            let uuids: HashSet<_> = affected.iter().map(|(uuid, _)| *uuid).collect();
            if matches!(source, Source::Player { uuid, .. } if uuids.contains(&uuid)) {
                disconnect = Some((IP_BANNED, IP_BANNED_TEXT));
            }
            logic
                .disconnect_players(uuids, IP_BANNED, IP_BANNED_TEXT)
                .map_err(io::Error::other)?;
            let names = affected
                .iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            replies.push((
                format!("This ban affects {} player(s): {names}", affected.len()),
                translate(
                    "commands.banip.info",
                    vec![
                        (affected.len() as i32).into(),
                        join_names(
                            affected
                                .into_iter()
                                .map(|(uuid, name)| player_name(*uuid, name.clone())),
                        ),
                    ],
                ),
            ));
        }
        Ok(Outcome::Feedback {
            replies,
            disconnect,
        })
    }

    fn pardon_ip(&self, target: &str) -> Outcome {
        if target.parse::<Ipv4Addr>().is_err() {
            return failure("Invalid IP address".into(), "commands.pardonip.invalid");
        }
        match self.access().change_ip_ban(target, None) {
            Ok(true) => feedback(
                format!("Unbanned IP {target}"),
                translate_strings("commands.pardonip.success", vec![target.into()]),
            ),
            Ok(false) => failure(
                "Nothing changed. That IP isn't banned".into(),
                "commands.pardonip.failed",
            ),
            Err(error) => ip_ban_write_failed(error),
        }
    }

    fn resolve_targets(
        &self,
        logic: &Handle,
        source: Source,
        target: &str,
        cursor: usize,
    ) -> io::Result<Result<Vec<Profile>, Outcome>> {
        Ok(if let Some(selector) = target.strip_prefix('@') {
            match self.select_players(logic, source, selector, true)? {
                Ok(profiles) if profiles.is_empty() => Err(failure(
                    "No player was found".into(),
                    "argument.entity.notfound.player",
                )),
                Ok(profiles) => Ok(profiles),
                Err(reason) => Err(Outcome::Syntax { cursor, reason }),
            }
        } else if let Some(profile) = self.profiles.resolve(target) {
            Ok(vec![profile])
        } else {
            Err(failure(
                "That player does not exist".into(),
                "argument.player.unknown",
            ))
        })
    }

    fn command_reason(
        &self,
        logic: &Handle,
        source: Source,
        reason: Option<&str>,
        input_len: usize,
    ) -> io::Result<Result<Option<String>, Outcome>> {
        let Some(reason) = reason else {
            return Ok(Ok(None));
        };
        Ok(match self.resolve_reason(logic, source, reason)? {
            Ok(reason) => Ok(Some(reason)),
            Err((offset, error)) => Err(Outcome::Syntax {
                cursor: input_len - reason.len() + offset,
                reason: error,
            }),
        })
    }

    fn resolve_reason(
        &self,
        logic: &Handle,
        source: Source,
        reason: &str,
    ) -> io::Result<Result<String, (usize, &'static str)>> {
        let mut remaining = reason;
        let mut text = String::with_capacity(reason.len());
        while let Some((prefix, candidate)) = remaining.split_once('@') {
            text.push_str(prefix);
            if !matches!(
                candidate.as_bytes().first(),
                Some(b'a' | b'p' | b'r' | b's' | b'e' | b'n')
            ) {
                text.push('@');
                remaining = candidate;
                continue;
            }
            let offset = reason.len() - candidate.len() - 1;
            let end = if candidate[1..].starts_with('[') {
                let Some(close) = candidate.find(']') else {
                    return Ok(Err((offset, "argument.entity.options.unterminated")));
                };
                close + 1
            } else {
                1
            };
            let players = match self.select_players(logic, source, &candidate[..end], false)? {
                Ok(players) => players,
                Err(error) => return Ok(Err((offset, error))),
            };
            for (index, profile) in players.into_iter().enumerate() {
                if index != 0 {
                    text.push_str(", ");
                }
                text.push_str(&profile.name);
            }
            remaining = &candidate[end..];
        }
        text.push_str(remaining);
        Ok(Ok(text))
    }

    fn select_players(
        &self,
        logic: &Handle,
        source: Source,
        selector: &str,
        profiles_only: bool,
    ) -> io::Result<Result<Vec<Profile>, &'static str>> {
        let kind = match selector.as_bytes().first().copied() {
            Some(kind @ (b'a' | b'p' | b'r' | b's' | b'e' | b'n')) => kind,
            Some(_) => return Ok(Err("argument.entity.selector.unknown")),
            None => return Ok(Err("argument.entity.selector.missing")),
        };
        let filters = &selector[1..];
        let mut player_type = false;
        let mut name_filter = None;
        if !filters.is_empty() {
            let Some(filters) = filters
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
            else {
                return Ok(Err("command.unknown.argument"));
            };
            for filter in filters.split(',').filter(|filter| !filter.is_empty()) {
                if matches!(filter, "type=player" | "type=minecraft:player") && !player_type {
                    player_type = true;
                } else if let Some(name) = filter.strip_prefix("name=")
                    && name_filter.is_none()
                {
                    name_filter = Some(name);
                } else {
                    return Ok(Err("command.unknown.argument"));
                }
            }
        }
        if profiles_only && matches!(kind, b's' | b'e' | b'n') && !player_type {
            return Ok(Err("argument.player.entities"));
        }

        let mut players = logic.player_list().map_err(io::Error::other)?;
        let source_player = match source {
            Source::Console => None,
            Source::Player { uuid, .. } => players.iter().find(|p| p.uuid == uuid),
        };
        let origin = source_player.map_or(self.spawn, |p| p.position);
        let sender = source_player.map(|p| p.uuid);
        players.retain(|player| {
            (kind != b's' || sender == Some(player.uuid))
                && name_filter.is_none_or(|name| {
                    name.strip_prefix('!')
                        .map_or(player.name == name, |name| player.name != name)
                })
        });
        match kind {
            b'p' | b'n' => {
                let distance = |position: [f64; 3]| {
                    position
                        .into_iter()
                        .zip(origin)
                        .map(|(a, b)| (a - b).powi(2))
                        .sum::<f64>()
                };
                players.sort_by(|a, b| distance(a.position).total_cmp(&distance(b.position)));
                players.truncate(1);
            }
            b'r' if !players.is_empty() => {
                let mut random = [0; 8];
                getrandom::fill(&mut random).map_err(io::Error::other)?;
                let index = (u64::from_ne_bytes(random) % players.len() as u64) as usize;
                players.swap(0, index);
                players.truncate(1);
            }
            _ => {}
        }
        Ok(Ok(players
            .into_iter()
            .map(|p| Profile {
                uuid: p.uuid,
                name: p.name,
            })
            .collect()))
    }

    fn reload_whitelist(&self, logic: &Handle, source: Source) -> io::Result<Outcome> {
        let mut access = self.access();
        if let Err(error) = access.reload_whitelist() {
            log::warn!("Failed to load white-list: {error}");
            return Ok(failure(
                "Could not reload the whitelist".into(),
                "command.failed",
            ));
        }
        let disconnect = Self::enforce_whitelist(&access, logic, source)?;
        Ok(Outcome::Feedback {
            replies: vec![(
                "Reloaded the whitelist".into(),
                translate("commands.whitelist.reloaded", vec![]),
            )],
            disconnect,
        })
    }

    fn enforce_whitelist(
        access: &AccessLists,
        logic: &Handle,
        source: Source,
    ) -> io::Result<Option<Disconnect>> {
        if !access.whitelist_enforced() {
            return Ok(None);
        }
        let allowed: HashSet<_> = access.whitelisted_players().collect();
        let disconnect = matches!(source, Source::Player { uuid, .. } if !allowed.contains(&uuid));
        logic
            .disconnect_players_except(allowed, NOT_WHITELISTED, NOT_WHITELISTED_TEXT)
            .map_err(io::Error::other)?;
        Ok(disconnect.then_some((NOT_WHITELISTED, NOT_WHITELISTED_TEXT)))
    }

    /// Returns the feedback and any disconnect reason for the sender.
    pub fn player_reply(
        &self,
        logic: &Handle,
        input: &str,
        source: Source,
    ) -> io::Result<(Vec<clientbound::SystemChatMessage>, Option<Disconnect>)> {
        let (components, disconnect) = match self.run(logic, input, source)? {
            Outcome::Feedback {
                replies,
                disconnect,
            } => (
                replies
                    .into_iter()
                    .map(|(_, component)| component)
                    .collect(),
                disconnect,
            ),
            Outcome::Syntax { cursor, reason } => (
                vec![
                    command_failure(Compound::from_iter([("translate", reason)])),
                    command_failure(error_context(input, cursor)),
                ],
                None,
            ),
            Outcome::Help | Outcome::Stop => unreachable!("console-only commands were filtered"),
        };
        Ok((
            components
                .into_iter()
                .map(|content| clientbound::SystemChatMessage {
                    content,
                    overlay: false,
                })
                .collect(),
            disconnect,
        ))
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

fn translate_strings(key: &str, args: Vec<String>) -> Nbt {
    Compound::from_iter([
        ("translate", Nbt::from(key)),
        ("with", List::String(args).into()),
    ])
    .into()
}

fn translate_ban(key: &str, mut args: Vec<String>, reason: Option<&str>) -> Nbt {
    if let Some(reason) = reason {
        args.push(reason.into());
        translate_strings(key, args)
    } else {
        let mut args: Vec<_> = args.into_iter().map(Nbt::from).collect();
        args.push(translate(DEFAULT_BAN_REASON_KEY, vec![]));
        translate(key, args)
    }
}

fn ip_ban_write_failed(error: io::Error) -> Outcome {
    log::warn!("Failed to save the IP ban list: {error}");
    failure("Could not save the IP ban list".into(), "command.failed")
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

fn failure(text: String, key: &str) -> Outcome {
    feedback(
        text,
        command_failure(Compound::from_iter([("translate", key)])),
    )
}

fn feedback(text: String, component: Nbt) -> Outcome {
    Outcome::Feedback {
        replies: vec![(text, component)],
        disconnect: None,
    }
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
