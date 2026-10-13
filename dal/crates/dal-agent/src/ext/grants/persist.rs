//! grants.toml persistence: load, parse, encode, atomic write.
//!
//! Rows persist through [`super::GrantStore`]; this module owns only the
//! file shape. A hand-edited file fails closed: malformed escapes reject
//! the value, unknown origins drop the row, and a missing file loads
//! empty. Writes go through the store's atomic writer at mode `0600`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dal_core::{ClientId, Name, Origin, Service, ServiceSet, Timestamp};
use dal_store::{FileMode, write_atomic};

use super::{GrantRow, GrantStoreError};

pub(crate) fn grants_path(data_dir: &Path) -> PathBuf {
    data_dir.join("grants.toml")
}

pub(crate) fn load(data_dir: &Path) -> Result<Vec<GrantRow>, GrantStoreError> {
    let bytes = match std::fs::read(grants_path(data_dir)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(GrantStoreError::Read(e)),
    };
    parse_rows(&String::from_utf8_lossy(&bytes))
}

/// Parses every row, failing on the first malformed line. Unknown fields,
/// unquoted values, unbracketed service lists, and unknown services all
/// fail: a hand-edited file fails closed instead of parsing sideways, and
/// callers never write back over a file they could not load.
pub(crate) fn parse_rows(text: &str) -> Result<Vec<GrantRow>, GrantStoreError> {
    let mut rows = Vec::new();
    let mut fields: BTreeMap<Box<str>, Box<str>> = BTreeMap::new();
    let mut services: Vec<Service> = Vec::new();
    let mut block_line = 0usize;
    let flush = |fields: &mut BTreeMap<Box<str>, Box<str>>,
                 services: &mut Vec<Service>,
                 block_line: usize,
                 rows: &mut Vec<GrantRow>|
     -> Result<(), GrantStoreError> {
        if fields.is_empty() && services.is_empty() {
            return Err(GrantStoreError::Malformed {
                detail: format!("line {block_line}: empty grant block").into(),
            });
        }
        let row = build_row(fields, services, block_line)?;
        fields.clear();
        services.clear();
        rows.push(row);
        Ok(())
    };
    for (n, raw) in text.lines().enumerate() {
        let no = n + 1;
        let line: &str = raw.trim();
        if line == "[[grant]]" {
            if block_line > 0 {
                flush(&mut fields, &mut services, block_line, &mut rows)?;
            }
            block_line = no;
            continue;
        }
        if block_line == 0 {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            return Err(GrantStoreError::Malformed {
                detail: format!("line {no}: expected [[grant]]").into(),
            });
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(GrantStoreError::Malformed {
                detail: format!("line {no}: expected key = value").into(),
            });
        };
        if k.trim() == "services" {
            let list = v.trim();
            let Some(inner) = list.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
                return Err(GrantStoreError::Malformed {
                    detail: format!("line {no}: services must be [\"a\", \"b\"]").into(),
                });
            };
            services = inner
                .split(',')
                .map(|s| {
                    let unquoted = unquote(s.trim()).ok_or_else(|| GrantStoreError::Malformed {
                        detail: format!("line {no}: bad service name").into(),
                    })?;
                    Service::parse(&unquoted).map_err(|_| GrantStoreError::Malformed {
                        detail: format!("line {no}: unknown service").into(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
        } else if matches!(
            k.trim(),
            "ext" | "origin" | "by" | "approved_at" | "mcp_set"
        ) {
            let unquoted = unquote(v.trim()).ok_or_else(|| GrantStoreError::Malformed {
                detail: format!("line {no}: value must be quoted").into(),
            })?;
            fields.insert(k.trim().into(), unquoted.into());
        } else {
            return Err(GrantStoreError::Malformed {
                detail: format!("line {no}: unknown field").into(),
            });
        }
    }
    if block_line > 0 {
        flush(&mut fields, &mut services, block_line, &mut rows)?;
    }
    Ok(rows)
}

fn build_row(
    fields: &BTreeMap<Box<str>, Box<str>>,
    services: &[Service],
    block_line: usize,
) -> Result<GrantRow, GrantStoreError> {
    let malformed = |what: &str| GrantStoreError::Malformed {
        detail: format!("line {block_line}: bad {what}").into(),
    };
    let ext: Name = fields
        .get("ext")
        .ok_or_else(|| malformed("grant without ext"))
        .and_then(|v| v.as_ref().parse().map_err(|_| malformed("ext")))?;
    let origin = match fields.get("origin").map(Box::as_ref) {
        Some("user") => Origin::User,
        Some("bundled") => Origin::Bundled,
        _ => return Err(malformed("origin")),
    };
    if services.is_empty() {
        return Err(malformed("services"));
    }
    let services = ServiceSet::from_names(services.iter().map(|service| service.as_str()))
        .map_err(|_| malformed("services"))?;
    let by = ClientId::new(fields.get("by").ok_or_else(|| malformed("by"))?.as_ref());
    let approved_at: Timestamp = fields
        .get("approved_at")
        .ok_or_else(|| malformed("approved_at"))
        .and_then(|v| v.as_ref().parse().map_err(|_| malformed("approved_at")))?;
    Ok(GrantRow {
        ext,
        origin,
        services,
        mcp_set: fields.get("mcp_set").cloned(),
        by,
        approved_at,
    })
}

pub(crate) fn encode_row(row: &GrantRow) -> String {
    let origin = match row.origin {
        Origin::User => "user",
        Origin::Bundled => "bundled",
        Origin::Builtin => "builtin",
        _ => "unknown",
    };
    let services = row
        .services
        .iter()
        .map(|s| format!("\"{}\"", escape(s.as_str())))
        .collect::<Vec<_>>()
        .join(", ");
    let mcp_set = row
        .mcp_set
        .as_deref()
        .map(|digest| format!("mcp_set = \"{}\"\n", escape(digest)))
        .unwrap_or_default();
    format!(
        "[[grant]]\next = \"{}\"\norigin = \"{origin}\"\nservices = [{services}]\n{mcp_set}by = \"{}\"\napproved_at = \"{}\"\n",
        escape(row.ext.as_str()),
        escape(row.by.as_str()),
        row.approved_at,
    )
}

pub(crate) fn persist(data_dir: &Path, rows: &[GrantRow]) -> Result<(), dal_store::StoreError> {
    let mut text = String::new();
    for row in rows {
        text.push_str(&encode_row(row));
    }
    write_atomic(&grants_path(data_dir), text.as_bytes(), FileMode::Mode0600)
}

/// Escapes one TOML basic-string body: a crafted client id must not break
/// out of its quotes and forge rows.
pub(crate) fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// Unquotes one TOML basic string, rejecting malformed escapes so a
/// hand-edited row fails closed instead of parsing sideways.
pub(crate) fn unquote(value: &str) -> Option<String> {
    unescape(value.strip_prefix('"')?.strip_suffix('"')?)
}

pub(crate) fn unescape(value: &str) -> Option<String> {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            _ => return None,
        }
    }
    Some(out)
}
