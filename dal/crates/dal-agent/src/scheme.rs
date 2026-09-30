//! URI parsing and the host's built-in document-scheme table.

use std::collections::BTreeMap;

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

pub(crate) struct SchemeTable {
    pub(crate) resolvers: BTreeMap<Box<str>, BuiltinResolver>,
}

impl SchemeTable {
    pub(crate) fn new() -> Self {
        Self {
            resolvers: BTreeMap::from([
                (Box::<str>::from("session"), BuiltinResolver::Session),
                (Box::<str>::from("job"), BuiltinResolver::Job),
            ]),
        }
    }

    pub(crate) fn resolver(&self, scheme: &str) -> Result<BuiltinResolver, SchemeError> {
        self.resolvers
            .get(scheme)
            .copied()
            .ok_or_else(|| SchemeError::Unknown {
                scheme: scheme.into(),
            })
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
