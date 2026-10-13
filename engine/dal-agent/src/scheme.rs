//! URI parsing and the host's built-in document-scheme lookup.

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
