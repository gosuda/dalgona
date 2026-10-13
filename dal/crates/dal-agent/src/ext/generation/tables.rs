//! Generation index tables: claim ledgers plus canonical lookup tables.
//!
//! [`ClaimTables`] is the validation ledger: seeded from the previous
//! generation, then fed batch extensions in canonical order so the first
//! owner wins. The lookup tables below index one built [`super::Generation`]
//! without cloning records; dispatch resolves positions through the
//! extensions slice.

use std::collections::HashMap;

use dal_core::{Claimant, CommandName, ModelId, Name, Origin, RegistrationError, Visibility};

use crate::ext::Extension;

use super::validate::{claimant_of, conflict_name};

/// Owner index over every `(record kind, name)` pair seen so far.
///
/// Seeded from the previous generation first, so a batch record shadowing a
/// live one reports the previous owner as claimant, then fed batch
/// extensions in canonical order so the first batch owner wins.
pub(crate) struct ClaimTables {
    tools: HashMap<Name, Claimant>,
    commands: HashMap<CommandName, Claimant>,
    skills: HashMap<Name, Claimant>,
    rules: HashMap<Name, Claimant>,
    models: HashMap<ModelId, Claimant>,
    schemes: HashMap<Box<str>, Claimant>,
    doc_schemes: HashMap<Box<str>, Claimant>,
    doc_uris: HashMap<Box<str>, Claimant>,
    compactors: HashMap<Box<str>, Claimant>,
    statuses: HashMap<Box<str>, Claimant>,
    mcp_owner: Option<Claimant>,
    evidence_owner: Option<Claimant>,
}

/// Claims one `(kind, name)` pair for `owner`, failing when already owned.
fn claim<K: Eq + std::hash::Hash>(
    map: &mut HashMap<K, Claimant>,
    kind: &'static str,
    key: K,
    name: &Name,
    owner: &Claimant,
) -> Result<(), RegistrationError> {
    if let Some(first) = map.get(&key) {
        return Err(RegistrationError::Conflict {
            kind,
            name: name.clone(),
            claimant: first.clone(),
        });
    }
    map.insert(key, owner.clone());
    Ok(())
}

impl ClaimTables {
    pub(crate) fn new() -> Self {
        Self {
            tools: HashMap::new(),
            commands: HashMap::new(),
            skills: HashMap::new(),
            rules: HashMap::new(),
            models: HashMap::new(),
            schemes: HashMap::new(),
            doc_schemes: HashMap::new(),
            doc_uris: HashMap::new(),
            compactors: HashMap::new(),
            statuses: HashMap::new(),
            mcp_owner: None,
            evidence_owner: None,
        }
    }

    /// Records every pair of one previous-generation extension without
    /// failing: the previous generation already validated clean.
    pub(crate) fn seed(&mut self, ext: &Extension) -> Result<(), RegistrationError> {
        let (owner, _) = claimant_of(ext)?;
        for (tool, _) in ext.tools() {
            self.tools.insert(tool.name().clone(), owner.clone());
        }
        for (spec, _) in ext.commands() {
            self.commands.insert(spec.name.clone(), owner.clone());
        }
        for skill in ext.skills() {
            self.skills.insert(skill.name.clone(), owner.clone());
        }
        for rule in ext.rules() {
            self.rules.insert(rule.name.clone(), owner.clone());
        }
        for record in ext.models() {
            self.models.insert(record.id.clone(), owner.clone());
        }
        for (name, _) in ext.schemes() {
            self.schemes.insert(name.clone(), owner.clone());
        }
        if !ext.docs().is_empty() {
            self.doc_schemes.insert(ext.name().into(), owner.clone());
            for record in ext.docs() {
                self.doc_uris
                    .insert(record.uri(ext.name()).into(), owner.clone());
            }
        }
        for (name, _) in ext.compactors() {
            self.compactors.insert(name.clone(), owner.clone());
        }
        for (kind, _) in &ext.statuses {
            self.statuses.insert(kind.clone(), owner.clone());
        }
        if self.evidence_owner.is_none() && !ext.evidence().is_empty() {
            self.evidence_owner = Some(owner.clone());
        }
        if self.mcp_owner.is_none() && !ext.mcp_clients().is_empty() {
            self.mcp_owner = Some(owner);
        }
        Ok(())
    }

    /// Claims every pair of one batch extension, failing on the first pair
    /// another extension already owns.
    pub(crate) fn check(&mut self, ext: &Extension) -> Result<(), RegistrationError> {
        let (owner, owner_name) = claimant_of(ext)?;
        for (tool, _) in ext.tools() {
            claim(
                &mut self.tools,
                "tool",
                tool.name().clone(),
                tool.name(),
                &owner,
            )?;
        }
        for (spec, _) in ext.commands() {
            let name = conflict_name(spec.name.as_str(), &owner_name);
            claim(
                &mut self.commands,
                "command",
                spec.name.clone(),
                &name,
                &owner,
            )?;
        }
        for skill in ext.skills() {
            claim(
                &mut self.skills,
                "skill",
                skill.name.clone(),
                &skill.name,
                &owner,
            )?;
        }
        for rule in ext.rules() {
            claim(
                &mut self.rules,
                "rule",
                rule.name.clone(),
                &rule.name,
                &owner,
            )?;
        }
        for record in ext.models() {
            let name = conflict_name(record.id.as_str(), &owner_name);
            claim(&mut self.models, "model", record.id.clone(), &name, &owner)?;
        }
        for (name, _) in ext.schemes() {
            let report = conflict_name(name, &owner_name);
            claim(&mut self.schemes, "scheme", name.clone(), &report, &owner)?;
            if let Some(first) = self.doc_schemes.get(name)
                && first != &owner
            {
                return Err(RegistrationError::Conflict {
                    kind: "scheme",
                    name: report,
                    claimant: first.clone(),
                });
            }
        }
        self.check_docs(ext, &owner, &owner_name)?;
        for (name, _) in ext.compactors() {
            let report = conflict_name(name, &owner_name);
            claim(
                &mut self.compactors,
                "compactor",
                name.clone(),
                &report,
                &owner,
            )?;
        }
        for (kind, _) in &ext.statuses {
            let report = conflict_name(kind, &owner_name);
            claim(&mut self.statuses, "status", kind.clone(), &report, &owner)?;
        }
        self.check_slots(ext, owner)
    }

    /// Claims the extension's doc scheme and every doc URI.
    fn check_docs(
        &mut self,
        ext: &Extension,
        owner: &Claimant,
        owner_name: &Name,
    ) -> Result<(), RegistrationError> {
        if ext.docs().is_empty() {
            return Ok(());
        }
        let scheme: Box<str> = ext.name().into();
        if let Some(first) = self.doc_schemes.get(&scheme) {
            return Err(RegistrationError::Conflict {
                kind: "scheme",
                name: owner_name.clone(),
                claimant: first.clone(),
            });
        }
        if let Some(first) = self.schemes.get(&scheme)
            && first != owner
        {
            return Err(RegistrationError::Conflict {
                kind: "scheme",
                name: owner_name.clone(),
                claimant: first.clone(),
            });
        }
        if (scheme.as_ref() == "dal" || scheme.as_ref() == "dalgona")
            && ext.origin() != Origin::Builtin
        {
            return Err(RegistrationError::Conflict {
                kind: "scheme",
                name: owner_name.clone(),
                claimant: Claimant::Builtin(owner_name.clone()),
            });
        }
        self.doc_schemes.insert(scheme, owner.clone());
        for record in ext.docs() {
            let uri: Box<str> = record.uri(ext.name()).into();
            if self.doc_uris.contains_key(&uri) {
                return Err(RegistrationError::DuplicateDocUri { uri });
            }
            self.doc_uris.insert(uri, owner.clone());
        }
        Ok(())
    }

    /// Claims the single-slot MCP client and evidence owner.
    fn check_slots(&mut self, ext: &Extension, owner: Claimant) -> Result<(), RegistrationError> {
        if !ext.evidence().is_empty() {
            let first = self.evidence_owner.clone();
            if first.is_some() || ext.evidence().len() > 1 {
                return Err(RegistrationError::Conflict {
                    kind: "evidence",
                    name: Name::parse(ext.name())?,
                    claimant: first.unwrap_or_else(|| owner.clone()),
                });
            }
            self.evidence_owner = Some(owner.clone());
        }
        if ext.mcp_clients().is_empty() {
            return Ok(());
        }
        if let Some(first) = self.mcp_owner.clone() {
            return Err(RegistrationError::Conflict {
                kind: "mcp_client",
                name: Name::parse(ext.name())?,
                claimant: first,
            });
        }
        if ext.mcp_clients().len() > 1 {
            return Err(RegistrationError::Conflict {
                kind: "mcp_client",
                name: Name::parse(ext.name())?,
                claimant: owner,
            });
        }
        self.mcp_owner = Some(owner);
        Ok(())
    }
}

/// One tool's position in [`super::Generation::extensions`] plus its model
/// visibility. The tool record itself never clones; dispatch resolves the
/// pair through the extensions slice.
pub(crate) struct ToolEntry {
    pub(crate) name: Name,
    pub(crate) ext: usize,
    pub(crate) record: usize,
    pub(crate) visibility: Visibility,
}

/// Tool index in canonical order with name lookup.
pub(crate) struct ToolTable {
    pub(crate) entries: Box<[ToolEntry]>,
}

impl ToolTable {
    /// Returns the entry for `name`, when registered.
    pub(crate) fn find(&self, name: &Name) -> Option<&ToolEntry> {
        self.entries.iter().find(|entry| entry.name == *name)
    }

    /// Borrows every entry in canonical order.
    pub(crate) fn entries(&self) -> &[ToolEntry] {
        &self.entries
    }
}

/// One command's position in [`super::Generation::extensions`].
pub(crate) struct CommandEntry {
    pub(crate) name: CommandName,
    pub(crate) ext: usize,
    pub(crate) record: usize,
}

/// Command index in canonical order with name lookup.
pub(crate) struct CommandTable {
    pub(crate) entries: Box<[CommandEntry]>,
}

impl CommandTable {
    /// Returns the entry for `name`, when registered.
    pub(crate) fn find(&self, name: &CommandName) -> Option<&CommandEntry> {
        self.entries.iter().find(|entry| entry.name == *name)
    }

    /// Borrows every entry in canonical order.
    pub(crate) fn entries(&self) -> &[CommandEntry] {
        &self.entries
    }
}

/// One named resolver's position in [`super::Generation::extensions`].
pub(crate) struct NamedEntry {
    pub(crate) name: Box<str>,
    pub(crate) ext: usize,
    pub(crate) record: usize,
}

/// Scheme index in canonical order with name lookup.
pub(crate) struct SchemeTable {
    pub(crate) entries: Box<[NamedEntry]>,
}

impl SchemeTable {
    /// Returns the entry for `name`, when registered.
    pub(crate) fn find(&self, name: &str) -> Option<&NamedEntry> {
        self.entries
            .iter()
            .find(|entry| entry.name.as_ref() == name)
    }
}

/// Compactor index in canonical order with name lookup.
pub(crate) struct CompactorTable {
    pub(crate) entries: Box<[NamedEntry]>,
}

impl CompactorTable {
    /// Returns the entry for `name`, when registered.
    pub(crate) fn find(&self, name: &str) -> Option<&NamedEntry> {
        self.entries
            .iter()
            .find(|entry| entry.name.as_ref() == name)
    }

    /// Borrows every entry in canonical order.
    pub(crate) fn entries(&self) -> &[NamedEntry] {
        &self.entries
    }
}
