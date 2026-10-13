//! Built-in manual resolver: URI grammar, lookup, index, and miss texts.

/// One embedded manual: a scheme namespace plus its sorted pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manual {
    /// URI scheme (for example `dal`).
    pub scheme: String,
    /// Owning extension name.
    pub plugin: String,
    /// Sorted `(page, text)` pairs.
    pub pages: Vec<(String, String)>,
}

/// All loaded manuals in sorted scheme order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocsSnapshot {
    /// Manuals sorted by scheme.
    pub manuals: Vec<Manual>,
}

/// Result of resolving one documentation URI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    /// A page with its URI, title, and full text.
    Page {
        /// Full URI (for example `dal://config`).
        uri: String,
        /// Title taken from the page's first line.
        title: String,
        /// Full page text.
        text: String,
    },
    /// A scheme index with its URI and rendered text.
    Index {
        /// Full URI (for example `dal://`).
        uri: String,
        /// Rendered index text.
        text: String,
    },
    /// A miss with the structured reason.
    Miss(Miss),
}

/// Structured reason for a failed documentation lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Miss {
    /// The text is not a document URI.
    NotAUri {
        /// The input text.
        text: String,
        /// Nearest known page URI, when one is close enough.
        nearest: Option<String>,
    },
    /// The scheme is unknown.
    NoScheme {
        /// The requested scheme.
        scheme: String,
        /// Sorted known scheme names.
        known: Vec<String>,
    },
    /// The page is missing on a known scheme.
    NoPage {
        /// The requested scheme.
        scheme: String,
        /// The full requested URI.
        uri: String,
        /// Nearest page URI on that scheme, when one is close enough.
        nearest: Option<String>,
    },
}

/// Returns whether `page` matches the lowercase, slash-separated page grammar.
#[must_use]
pub fn page_valid(page: &str) -> bool {
    crate::docsgen::page_valid(page)
}

/// Returns whether `scheme` matches the lowercase scheme grammar.
#[must_use]
pub fn scheme_valid(scheme: &str) -> bool {
    if scheme.is_empty() {
        return false;
    }
    let mut bytes = scheme.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Returns the nearest candidate within edit distance 2, or `None`.
///
/// Candidates must arrive sorted; ties keep the earlier candidate because only a
/// strictly smaller distance replaces the current best.
#[must_use]
pub fn nearest(candidates_sorted: &[&str], text: &str) -> Option<String> {
    nearest_candidate(
        &candidates_sorted
            .iter()
            .map(|candidate| (*candidate, *candidate))
            .collect::<Vec<_>>(),
        text,
    )
}

fn nearest_candidate(pairs_sorted: &[(&str, &str)], text: &str) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for (key, uri) in pairs_sorted {
        let distance = capped_distance(key, text, 2);
        if distance <= 2 && best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, uri));
        }
    }
    best.map(|(_, uri)| uri.to_owned())
}

fn capped_distance(left: &str, right: &str, cap: usize) -> usize {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (row, &left_byte) in left.iter().enumerate() {
        current[0] = row + 1;
        let mut row_min = current[0];
        for (column, &right_byte) in right.iter().enumerate() {
            let cost = usize::from(left_byte != right_byte);
            current[column + 1] = (previous[column] + cost)
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
            row_min = row_min.min(current[column + 1]);
        }
        if row_min > cap {
            return cap + 1;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

/// Resolves one documentation URI against a snapshot.
///
/// This is a pure function of the snapshot: it owns no lock, thread, or external wait.
#[must_use]
pub fn lookup(snapshot: &DocsSnapshot, text: &str) -> Lookup {
    let Some(separator) = text.find("://") else {
        let pairs: Vec<(String, String)> = snapshot
            .manuals
            .iter()
            .flat_map(|manual| {
                manual
                    .pages
                    .iter()
                    .map(|(page, _)| (page.clone(), format!("{}://{page}", manual.scheme)))
            })
            .collect();
        let borrowed: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(page, uri)| (page.as_str(), uri.as_str()))
            .collect();
        return Lookup::Miss(Miss::NotAUri {
            text: text.to_owned(),
            nearest: nearest_candidate(&borrowed, text),
        });
    };
    let (scheme, page) = text.split_at(separator);
    let page = &page["://".len()..];
    let Some(manual) = snapshot
        .manuals
        .iter()
        .find(|manual| manual.scheme == scheme)
    else {
        let mut known: Vec<String> = snapshot
            .manuals
            .iter()
            .map(|manual| manual.scheme.clone())
            .collect();
        known.sort();
        return Lookup::Miss(Miss::NoScheme {
            scheme: scheme.to_owned(),
            known,
        });
    };
    if page.is_empty() {
        return Lookup::Index {
            uri: format!("{scheme}://"),
            text: render_index(manual),
        };
    }
    if !page_valid(page) {
        return page_miss(manual, text);
    }
    match manual.pages.iter().find(|(name, _)| name == page) {
        Some((_, body)) => Lookup::Page {
            uri: text.to_owned(),
            title: page_title(body),
            text: body.clone(),
        },
        None => page_miss(manual, text),
    }
}

fn page_miss(manual: &Manual, text: &str) -> Lookup {
    let pairs: Vec<(String, String)> = manual
        .pages
        .iter()
        .map(|(page, _)| (page.clone(), format!("{}://{page}", manual.scheme)))
        .collect();
    let borrowed: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(page, uri)| (page.as_str(), uri.as_str()))
        .collect();
    let page = text.split_once("://").map_or(text, |(_, page)| page);
    Lookup::Miss(Miss::NoPage {
        scheme: manual.scheme.clone(),
        uri: text.to_owned(),
        nearest: nearest_candidate(&borrowed, page),
    })
}

fn page_title(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .strip_prefix("# ")
        .unwrap_or_default()
        .to_owned()
}

/// Renders one scheme index.
///
/// Line 1 is `# <scheme>://`; each entry URI is padded to the longest entry URI plus
/// two spaces, then the page title. The text ends with exactly one newline.
#[must_use]
pub fn render_index(manual: &Manual) -> String {
    let mut output = format!("# {}://\n", manual.scheme);
    let width = manual
        .pages
        .iter()
        .map(|(page, _)| format!("{}://{page}", manual.scheme).len())
        .max()
        .unwrap_or(0)
        + 2;
    for (page, text) in &manual.pages {
        let uri = format!("{}://{page}", manual.scheme);
        let padding = " ".repeat(width.saturating_sub(uri.len()));
        output.push_str(&uri);
        output.push_str(&padding);
        output.push_str(&page_title(text));
        output.push('\n');
    }
    output
}

/// Renders the one- or two-line CLI message for a miss.
#[must_use]
pub fn miss_lines(miss: &Miss, product: &str) -> Vec<String> {
    match miss {
        Miss::NotAUri { text, nearest } => {
            let mut lines = vec![format!("{product}: \"{text}\" is not a document URI")];
            match nearest {
                Some(uri) => lines.push(format!(
                    "Did you mean {uri}? Run {product} docs for the list."
                )),
                None => lines.push(format!("Run {product} docs for the list.")),
            }
            lines
        }
        Miss::NoScheme { scheme, known } => {
            let manuals = known
                .iter()
                .map(|name| format!("{name}://"))
                .collect::<Vec<_>>()
                .join(", ");
            vec![
                format!("{product}: no documents at {scheme}://"),
                format!("Manuals: {manuals}. Run {product} docs for the list."),
            ]
        }
        Miss::NoPage { uri, nearest, .. } => {
            // Pinned CLI literal keeps the binary name as prefix but the product
            // name in the middle: `dalgon: no dal document at ...`.
            let document = if product == "dalgon" { "dal" } else { product };
            let mut lines = vec![format!("{product}: no {document} document at {uri}")];
            match nearest {
                Some(uri) => lines.push(format!(
                    "Did you mean {uri}? Run {product} docs for the list."
                )),
                None => lines.push(format!("Run {product} docs for the list.")),
            }
            lines
        }
    }
}

/// Renders the one-line read-tool message for a page miss.
#[must_use]
pub fn read_miss_line(miss: &Miss) -> Option<String> {
    match miss {
        Miss::NoPage {
            scheme,
            uri,
            nearest,
        } => Some(match nearest {
            Some(near) => {
                format!(
                    "read: {uri} does not exist. Did you mean {near}? Read {scheme}:// for the index."
                )
            }
            None => format!("read: {uri} does not exist. Read {scheme}:// for the index."),
        }),
        _ => None,
    }
}

/// Maps a miss to its wire error code, message, and optional hint.
#[must_use]
pub fn wire_error(miss: &Miss) -> (i64, String, Option<String>) {
    match miss {
        Miss::NotAUri { text, nearest } => (
            -32602,
            format!("\"{text}\" is not a document URI"),
            nearest.as_ref().map(|uri| format!("Did you mean {uri}?")),
        ),
        Miss::NoScheme { scheme, .. } => (-32002, format!("no documents at {scheme}://"), None),
        Miss::NoPage { uri, nearest, .. } => (
            -32002,
            format!("no document at {uri}"),
            nearest.as_ref().map(|uri| format!("Did you mean {uri}?")),
        ),
    }
}

/// Builds the exact manual line appended to the system prompt.
#[must_use]
pub fn prompt_line(snapshot: &DocsSnapshot, product: &str) -> String {
    let list = snapshot
        .manuals
        .iter()
        .map(|manual| format!("{}:// ({})", manual.scheme, manual.plugin))
        .collect::<Vec<_>>()
        .join(", ");
    let has_dal = snapshot.manuals.iter().any(|manual| manual.scheme == "dal");
    let has_dala = snapshot
        .manuals
        .iter()
        .any(|manual| manual.scheme == "dalgona");
    if has_dal && snapshot.manuals.len() == 1 {
        "Manuals: dal:// (dal). Read a manual when the user asks about dal itself, its settings, or its plugins: read its index first, such as dal://, then read the whole page you need. Before you write or port a plugin, read dal://plugins and dal://convert-pi.".to_owned()
    } else if has_dal && has_dala && snapshot.manuals.len() == 2 {
        "Manuals: dalgona:// (dalgona), dal:// (dal). Read a manual when the user asks about dalgona itself, its settings, or its plugins: read its index first, such as dal://, then read the whole page you need. Before you write or port a plugin, read dal://plugins and dal://convert-pi.".to_owned()
    } else {
        format!(
            "Manuals: {list}. Read a manual when the user asks about {product} itself, its settings, or its plugins: read its index first, then read the whole page you need."
        )
    }
}

/// Renders every manual index in sorted scheme order.
#[must_use]
pub fn listing(snapshot: &DocsSnapshot) -> String {
    snapshot.manuals.iter().map(render_index).collect()
}
/// Generated manual pages `(page, text)` sorted by page in byte order.
mod generated {
    include!(concat!(env!("OUT_DIR"), "/dal_docs_pages.rs"));
}

/// Builds the live `dal` manual snapshot from the generated pages.
#[must_use]
pub fn snapshot() -> DocsSnapshot {
    let mut pages: Vec<(String, String)> = generated::PAGES
        .iter()
        .map(|(page, text)| ((*page).to_owned(), (*text).to_owned()))
        .collect();
    pages.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    DocsSnapshot {
        manuals: vec![Manual {
            scheme: "dal".to_owned(),
            plugin: "dal".to_owned(),
            pages,
        }],
    }
}

/// Scheme resolver serving one extension's manual, such as `dal://` or
/// `dalgona://`.
///
/// Pages come from the generation's published doc records (registered through
/// the builder's `doc`), not from a private table: the builder is the one
/// registry. Records publish under the extension scheme, so lookups use the
/// full `<scheme>://` URI. Lookup and miss texts are shared with the snapshot
/// path through [`lookup`] and [`read_miss_line`]. An extension that
/// registers doc pages registers this resolver under its own name, so a model
/// reads the pages with the `read` tool.
#[derive(Clone, Debug)]
pub struct ManualScheme {
    scheme: String,
}

impl ManualScheme {
    /// Serves the doc pages published under `scheme`.
    #[must_use]
    pub fn new(scheme: &str) -> Self {
        Self {
            scheme: scheme.to_owned(),
        }
    }
}

impl dal_agent::ext::SchemeResolver for ManualScheme {
    fn read<'a>(
        &'a self,
        path: &'a str,
        cx: &'a dal_agent::ext::SchemeCx<'a>,
    ) -> dal_agent::ext::BoxFuture<'a, Result<dal_agent::ext::Doc, dal_agent::error::SchemeError>>
    {
        Box::pin(async move {
            let prefix = format!("{}://", self.scheme);
            let uri = format!("{prefix}{path}");
            let mut pages: Vec<(String, String)> = Vec::new();
            for page in cx.docs() {
                if let Some(record_path) = page.uri.strip_prefix(prefix.as_str()) {
                    pages.push((record_path.to_owned(), page.text.to_string()));
                }
            }
            pages.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
            let snap = DocsSnapshot {
                manuals: vec![Manual {
                    scheme: self.scheme.clone(),
                    plugin: self.scheme.clone(),
                    pages,
                }],
            };
            match lookup(&snap, &uri) {
                Lookup::Page { uri, text, .. } | Lookup::Index { uri, text } => {
                    Ok(dal_agent::ext::Doc::new(uri, text))
                }
                Lookup::Miss(miss) => match read_miss_line(&miss) {
                    // The miss text is the scheme's ordinary product text, so
                    // the read tool prints the fixed hint, not a bare miss.
                    Some(hint) => Err(dal_agent::error::SchemeError::Failed {
                        message: hint.into(),
                    }),
                    None => Err(dal_agent::error::SchemeError::NotFound { uri: uri.into() }),
                },
            }
        })
    }
}

/// Registers the built-in manual as first-party extension `dal`.
///
/// Every generated page is registered as a doc record through the builder,
/// served as `dal://<page>`; the scheme resolver above reads those same
/// generation records, so the builder is the one registry.
///
/// # Errors
///
/// Returns the builder's registration error for an invalid identity.
pub fn extension() -> Result<dal_agent::ext::Extension, dal_core::RegistrationError> {
    let mut builder = dal_agent::ext::ExtensionBuilder::new(
        "dal",
        env!("CARGO_PKG_VERSION"),
        dal_core::ServiceSet::EMPTY,
    )?;
    for &(page, text) in generated::PAGES {
        builder = builder.doc(page, &page_title(text), text);
    }
    builder
        .scheme("dal", std::sync::Arc::new(ManualScheme::new("dal")))
        .build()
}
