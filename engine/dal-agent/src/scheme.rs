//! URI parsing and the host's built-in document-scheme lookup.

use dal_core::JobId;

use crate::error::SchemeError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SchemeUri<'a> {
    pub(crate) scheme: &'a str,
    pub(crate) path: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BuiltinResolver {
    Session,
    Job,
}

pub(crate) fn builtin_resolver(scheme: &str) -> Result<BuiltinResolver, SchemeError> {
    match scheme {
        "session" => Ok(BuiltinResolver::Session),
        "job" => Ok(BuiltinResolver::Job),
        _ => Err(SchemeError::Unknown {
            scheme: scheme.into(),
        }),
    }
}

/// Splits a `job://<id>` or `job://<id>/log` URI into its job identity and route.
///
/// [`JobId`] is minted once per spawn, so an id names at most one live
/// session's row: callers must ask every live table and honor the single
/// claim, never first-match across sessions. Unparseable ids are unknown
/// documents, matching the dispatch table's own outcome.
pub(crate) fn job_parts(uri: &str) -> Result<(JobId, &str), SchemeError> {
    let rest = uri
        .strip_prefix("job://")
        .ok_or_else(|| SchemeError::NotFound { uri: uri.into() })?;
    let (id_text, route) = rest
        .split_once('/')
        .map_or((rest, ""), |(id, route)| (id, route));
    let id = JobId::parse(id_text).map_err(|_| SchemeError::NotFound { uri: uri.into() })?;
    Ok((id, route))
}

pub(crate) fn parse(uri: &str) -> Result<SchemeUri<'_>, SchemeError> {
    let Some((scheme, path)) = uri.split_once("://") else {
        return Err(SchemeError::Unknown { scheme: uri.into() });
    };
    if scheme.is_empty() {
        return Err(SchemeError::Unknown {
            scheme: scheme.into(),
        });
    }
    Ok(SchemeUri { scheme, path })
}
