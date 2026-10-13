//! Strict decode of the `mcp` object in a `SKILL.md` front matter block.
//!
//! A skill body is the whole file, byte for byte. This decode only reads the
//! optional block between a first-line `---` and the next `---` line, and
//! only its top-level `mcp` key; every other top-level key is ignored. Below
//! `mcp` nothing is tolerated: an unknown key, a duplicate key, a wrong
//! shape, or a bad value is a load error named `path:line:col`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::mcp::{
    McpBlock, McpBlockError, McpServerDecl, ServerShapeError, ServerWire, validate_block,
};
use tree::{Entry, Pos, Value, content_lines, parse_map};

mod tree;

#[cfg(test)]
mod tests;

const FENCE: &str = "---";
const MCP_KEYS: &str = "servers";
const SERVER_KEYS: &str = "command, env, url";

pub(super) type Res<T> = Result<T, Fault>;

/// A located decode failure before the path is attached.
#[derive(Debug)]
pub(super) struct Fault {
    pub(super) line: u32,
    pub(super) col: u32,
    pub(super) kind: Kind,
}

/// A skill front matter block the decode rejects, at `path:line:col`.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{}:{line}:{col}: {kind}", path.display())]
pub struct SkillFrontError {
    /// The file the block came from.
    pub path: PathBuf,
    /// The 1-based line of the offending token.
    pub line: u32,
    /// The 1-based column, in characters, of the offending token.
    pub col: u32,
    /// What is wrong at that position.
    pub kind: Kind,
}

/// Why a skill front matter block is rejected.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum Kind {
    /// The block opens with `---` but no closing `---` line follows.
    #[error("the front matter has no closing \"---\" line")]
    Unclosed,
    /// Indentation contains a tab.
    #[error("indentation must use spaces")]
    Tab,
    /// A line is indented differently from its siblings.
    #[error("unexpected indentation")]
    Indent,
    /// A line inside `mcp` is not `key: value`.
    #[error("expected \"key: value\"")]
    NotAKey,
    /// Text follows a complete value.
    #[error("unexpected text after the value")]
    Trailing,
    /// A double-quoted string uses an unknown escape.
    #[error("unknown escape in a double-quoted string")]
    BadEscape,
    /// A quoted string never closes.
    #[error("unterminated quoted string")]
    UnterminatedQuote,
    /// A `[...]` list is malformed.
    #[error("malformed list; write [\"a\", \"b\"] on one line")]
    BadList,
    /// A key appears twice in one object.
    #[error("\"{key}\" appears twice")]
    Duplicate {
        /// The repeated key.
        key: Box<str>,
    },
    /// A key is not one the object accepts.
    #[error("unknown key \"{key}\"; expected {expected}")]
    UnknownKey {
        /// The rejected key.
        key: Box<str>,
        /// The accepted keys.
        expected: &'static str,
    },
    /// A key is missing.
    #[error("\"{key}\" is required")]
    Missing {
        /// The missing key.
        key: &'static str,
    },
    /// A value has the wrong shape.
    #[error("\"{key}\" must be {want}")]
    Expected {
        /// The key whose value is wrong.
        key: Box<str>,
        /// The accepted shape.
        want: &'static str,
    },
    /// A server object names no transport, both, or `env` with `url`.
    #[error("mcp server \"{server}\": {shape}")]
    Shape {
        /// The server name.
        server: Box<str>,
        /// The transport rule broken.
        shape: ServerShapeError,
    },
    /// A server entry breaks a value rule.
    #[error("{0}")]
    Value(McpBlockError),
}

pub(super) fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

pub(super) fn pos_col(text: &str, byte: usize, indent: usize) -> u32 {
    to_u32(indent + text[..byte].chars().count() + 1)
}

/// Decodes the `mcp` object of a `SKILL.md` front matter block.
///
/// Returns `Ok(None)` when the file has no block or the block has no `mcp`
/// key. `path` only names the file in errors.
///
/// # Errors
///
/// Returns the first [`SkillFrontError`] in file order.
pub fn decode_skill_mcp(path: &Path, source: &str) -> Result<Option<McpBlock>, SkillFrontError> {
    decode(source).map_err(|fault| SkillFrontError {
        path: path.to_path_buf(),
        line: fault.line,
        col: fault.col,
        kind: fault.kind,
    })
}

fn decode(source: &str) -> Res<Option<McpBlock>> {
    let text = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut source_lines = text.split('\n');
    let Some(opening) = source_lines.next() else {
        return Ok(None);
    };
    if opening.strip_suffix('\r').unwrap_or(opening) != FENCE {
        return Ok(None);
    }
    let mut header = Vec::new();
    let mut closed = false;
    for (index, source_line) in source_lines.enumerate() {
        if source_line.strip_suffix('\r').unwrap_or(source_line) == FENCE {
            closed = true;
            break;
        }
        let line = source_line.strip_suffix('\r').unwrap_or(source_line);
        header.push((to_u32(index + 2), line));
    }
    if !closed {
        return Err(Fault {
            line: 1,
            col: 1,
            kind: Kind::Unclosed,
        });
    }
    match mcp_lines(&header)? {
        Some(found) => block(&found).map(Some),
        None => Ok(None),
    }
}

struct McpLines<'a> {
    no: u32,
    text: &'a str,
    children: Vec<(u32, &'a str)>,
}

fn mcp_lines<'a>(header: &[(u32, &'a str)]) -> Res<Option<McpLines<'a>>> {
    let mut found: Option<McpLines<'a>> = None;
    let mut open = false;
    for &(no, raw) in header {
        let content = raw.trim_start();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        if !raw.starts_with([' ', '\t']) {
            open = is_mcp_key(raw);
            if !open {
                continue;
            }
            if found.is_some() {
                return Err(duplicate("mcp", no, 1));
            }
            found = Some(McpLines {
                no,
                text: raw.trim_end(),
                children: Vec::new(),
            });
        } else if open && let Some(current) = found.as_mut() {
            current.children.push((no, raw));
        }
    }
    Ok(found)
}

fn is_mcp_key(text: &str) -> bool {
    text.strip_prefix("mcp")
        .is_some_and(|rest| rest.trim_start_matches(' ').starts_with(':'))
}

fn duplicate(key: &str, line: u32, col: u32) -> Fault {
    Fault {
        line,
        col,
        kind: Kind::Duplicate { key: key.into() },
    }
}

fn block(found: &McpLines<'_>) -> Res<McpBlock> {
    let key_at = Pos {
        line: found.no,
        col: 1,
    };
    let after_key = found.text["mcp".len()..].trim_start_matches(' ');
    let inline = after_key.strip_prefix(':').unwrap_or_default();
    let body = inline.trim_start_matches(' ');
    let want = "a block mapping with a \"servers\" key";
    if !body.is_empty() && !body.starts_with('#') {
        let byte = found.text.len() - body.len();
        return Err(at(
            Pos {
                line: found.no,
                col: pos_col(found.text, byte, 0),
            },
            expected("mcp", want),
        ));
    }
    let children = content_lines(found.children.iter().copied())?;
    if children.is_empty() {
        return Err(at(key_at, expected("mcp", want)));
    }
    let entries = parse_map(&children)?;
    let map = server_map(servers_entry(&entries, key_at)?)?;
    let block = McpBlock {
        servers: servers_from(map)?,
    };
    if let Some(first) = validate_block(&block).err().into_iter().flatten().next() {
        return Err(value_fault(first, map));
    }
    Ok(block)
}

fn expected(key: &str, want: &'static str) -> Kind {
    Kind::Expected {
        key: key.into(),
        want,
    }
}

fn at(pos: Pos, kind: Kind) -> Fault {
    Fault {
        line: pos.line,
        col: pos.col,
        kind,
    }
}

fn unique(entries: &[Entry]) -> Res<()> {
    for (index, entry) in entries.iter().enumerate() {
        if entries[..index].iter().any(|prior| prior.key == entry.key) {
            return Err(at(
                entry.key_at,
                Kind::Duplicate {
                    key: entry.key.clone(),
                },
            ));
        }
    }
    Ok(())
}

fn servers_entry(entries: &[Entry], mcp_at: Pos) -> Res<&Entry> {
    unique(entries)?;
    if let Some(unknown) = entries.iter().find(|entry| &*entry.key != MCP_KEYS) {
        return Err(at(
            unknown.key_at,
            Kind::UnknownKey {
                key: unknown.key.clone(),
                expected: MCP_KEYS,
            },
        ));
    }
    entries
        .first()
        .ok_or_else(|| at(mcp_at, Kind::Missing { key: "servers" }))
}

fn server_map(entry: &Entry) -> Res<&[Entry]> {
    match &entry.value {
        Value::Map(entries) => {
            unique(entries)?;
            Ok(entries)
        }
        _ => Err(at(
            entry.value_at,
            expected("servers", "a mapping of server names"),
        )),
    }
}

fn servers_from(map: &[Entry]) -> Res<BTreeMap<Box<str>, McpServerDecl>> {
    let mut out = BTreeMap::new();
    for entry in map {
        let Value::Map(fields) = &entry.value else {
            return Err(at(entry.value_at, expected(&entry.key, "a mapping")));
        };
        let wire = server_wire(fields)?;
        let decl = McpServerDecl::try_from(wire).map_err(|shape| {
            at(
                entry.key_at,
                Kind::Shape {
                    server: entry.key.clone(),
                    shape,
                },
            )
        })?;
        out.insert(entry.key.clone(), decl);
    }
    Ok(out)
}

fn server_wire(fields: &[Entry]) -> Res<ServerWire> {
    unique(fields)?;
    let mut wire = ServerWire::default();
    for field in fields {
        match &*field.key {
            "command" => wire.command = Some(command_of(field)?),
            "env" => wire.env = Some(env_of(field)?),
            "url" => wire.url = Some(url_of(field)?),
            _ => {
                return Err(at(
                    field.key_at,
                    Kind::UnknownKey {
                        key: field.key.clone(),
                        expected: SERVER_KEYS,
                    },
                ));
            }
        }
    }
    Ok(wire)
}

fn command_of(field: &Entry) -> Res<Vec<Box<str>>> {
    match &field.value {
        Value::Seq(items) => Ok(items.clone()),
        _ => Err(at(
            field.value_at,
            expected("command", "a list of strings such as [\"npx\", \"server\"]"),
        )),
    }
}

fn url_of(field: &Entry) -> Res<Box<str>> {
    match &field.value {
        Value::Scalar(url) => Ok(url.clone()),
        _ => Err(at(field.value_at, expected("url", "a string"))),
    }
}

fn env_of(field: &Entry) -> Res<BTreeMap<Box<str>, Box<str>>> {
    let Value::Map(entries) = &field.value else {
        return Err(at(field.value_at, expected("env", "a mapping of strings")));
    };
    unique(entries)?;
    let mut env = BTreeMap::new();
    for entry in entries {
        let Value::Scalar(value) = &entry.value else {
            return Err(at(entry.value_at, expected(&entry.key, "a string")));
        };
        env.insert(entry.key.clone(), value.clone());
    }
    Ok(env)
}

fn value_fault(first: McpBlockError, map: &[Entry]) -> Fault {
    let pos = map
        .iter()
        .find(|entry| *entry.key == *first.server())
        .map_or(Pos { line: 1, col: 1 }, |entry| entry.key_at);
    at(pos, Kind::Value(first))
}
