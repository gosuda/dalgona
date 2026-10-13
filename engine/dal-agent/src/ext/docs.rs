//! Extension doc records: one page per record, served under the extension's
//! own scheme.
//!
//! The scheme is fixed to the extension name. At registration the scheme
//! must be outside the host-reserved namespaces and inside the scheme
//! grammar, and every page path must be inside the page grammar; a URI
//! registered twice in one generation rejects the candidate.

use dal_core::{RegistrationError, Site};

/// Schemes no plugin may publish doc pages under.
///
/// `job://` refs come from the job table and `rule://` reads come from the
/// rule files, both parsed by the host outside the resolver registry.
/// `letter`, `blob` and `session` are host-owned namespaces with no
/// resolver seat, so the claim tables cannot see them. `dal` and
/// `dalgona` stay off this list: the product manuals register through this
/// same builder, and user plugins are barred from those two schemes by the
/// origin check in the generation claim tables instead.
pub(crate) const RESERVED_DOC_SCHEMES: &[&str] = &["job", "letter", "rule", "blob", "session"];

/// One page contributed by an extension, with its registration call site.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocRecord {
    /// The page path, such as `plugins` or `examples/hello`.
    pub path: Box<str>,
    /// The page title.
    pub title: Box<str>,
    /// The page text.
    pub text: Box<str>,
    /// The `doc()` call site for error spans.
    pub site: Site,
}

impl DocRecord {
    /// Captures one page with the caller's source location.
    pub(crate) fn capture(path: &str, title: &str, text: &str, site: Site) -> Self {
        Self {
            path: path.into(),
            title: title.into(),
            text: text.into(),
            site,
        }
    }

    /// Returns the record's full URI.
    #[must_use]
    pub fn uri(&self, scheme: &str) -> String {
        format!("{scheme}://{}", self.path)
    }
}

/// A page published in one generation snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocPage {
    /// The full URI, `<scheme>://<path>`.
    pub uri: Box<str>,
    /// The page title.
    pub title: Box<str>,
    /// The page text.
    pub text: Box<str>,
}

/// Every doc page of one generation, sorted by URI.
#[derive(Clone, Debug, Default)]
pub(crate) struct DocTable {
    pages: Box<[DocPage]>,
}

impl DocPage {
    /// Publishes one record under its extension scheme.
    pub(crate) fn of(scheme: &str, record: &DocRecord) -> Self {
        Self {
            uri: record.uri(scheme).into(),
            title: record.title.clone(),
            text: record.text.clone(),
        }
    }
}

impl DocTable {
    pub(crate) fn publish(mut pages: Vec<DocPage>) -> Self {
        pages.sort_by(|a, b| a.uri.cmp(&b.uri));
        Self {
            pages: pages.into_boxed_slice(),
        }
    }

    /// Looks one page up by its full URI.
    #[must_use]
    pub(crate) fn find(&self, uri: &str) -> Option<&DocPage> {
        self.pages.iter().find(|page| page.uri.as_ref() == uri)
    }

    /// Lists every page in URI order.
    #[must_use]
    pub(crate) fn list(&self) -> &[DocPage] {
        &self.pages
    }
}

fn valid_scheme_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && name.len() <= 64
}

fn valid_page_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|segment| {
            let mut bytes = segment.bytes();
            matches!(bytes.next(), Some(b'a'..=b'z' | b'0'..=b'9'))
                && bytes
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

/// Rejects a reserved doc scheme.
pub(crate) fn check_scheme(name: &str, site: &Site) -> Result<(), RegistrationError> {
    if RESERVED_DOC_SCHEMES.contains(&name) {
        return Err(RegistrationError::DocSchemeReserved {
            scheme: name.into(),
            site: site.clone(),
        });
    }
    if !valid_scheme_name(name) {
        return Err(RegistrationError::InvalidDocScheme {
            scheme: name.into(),
            site: site.clone(),
        });
    }
    Ok(())
}

/// Rejects a page path outside the page grammar.
pub(crate) fn check_path(record: &DocRecord) -> Result<(), RegistrationError> {
    if !valid_page_path(&record.path) {
        return Err(RegistrationError::InvalidDocPage {
            path: record.path.clone(),
            site: record.site.clone(),
        });
    }
    Ok(())
}
