//! Explicit per-record replacement settlement over one declared batch.
//!
//! A user plugin may take one bundled `skill` or `rule` record by naming the
//! bundled extension that owns it. [`settle`] is the one engine-owned, pure
//! settlement product assembly and `/reload` both run over the declared
//! batch: it validates every claim against the declared owners, prunes only
//! the taken records from the bundled extensions that own them, then runs
//! the canonical record-claim check over what remains, so every refusal
//! surfaces at load instead of at the first host publish. It owns no
//! registry and no grant store, and it never touches a service request,
//! tool, hook, status kind, prompt section, or grant: the replacing plugin
//! keeps its own identity and the battery keeps everything but the one
//! record.

use std::collections::BTreeMap;

use dal_core::{Name, Origin, RegistrationError, ReplaceClaim, ReplaceKind, ReplaceRefusal, Site};

use super::Extension;
use super::generation::{ClaimTables, canonical_order, claimant_of};

/// A refused settlement: the typed refusal plus the batch at the point of
/// refusal, so the caller can locate the conflicting plugin's declaration
/// site.
///
/// The batch is unchanged for a refused claim and already pruned for a
/// remaining record clash; every extension keeps its name, origin, and site
/// either way.
#[derive(Debug)]
pub struct Refused {
    /// The typed engine refusal.
    pub error: RegistrationError,
    /// The batch at the point of refusal.
    pub extensions: Vec<Extension>,
}

/// Who took one record: the bundled extension that owned it and the plugin
/// that replaced it.
struct Holder {
    owner: Name,
    replacer: Name,
}

/// The settled replacement ledger: every taken record by kind and name.
#[derive(Default)]
struct Taken {
    skills: BTreeMap<Name, Holder>,
    rules: BTreeMap<Name, Holder>,
}

impl Taken {
    fn of(&self, kind: ReplaceKind) -> &BTreeMap<Name, Holder> {
        match kind {
            ReplaceKind::Skill => &self.skills,
            ReplaceKind::Rule => &self.rules,
        }
    }

    fn of_mut(&mut self, kind: ReplaceKind) -> &mut BTreeMap<Name, Holder> {
        match kind {
            ReplaceKind::Skill => &mut self.skills,
            ReplaceKind::Rule => &mut self.rules,
        }
    }

    fn is_empty(&self) -> bool {
        self.skills.is_empty() && self.rules.is_empty()
    }

    /// Whether the ledger took `record` of `kind` from the extension named
    /// `owner`. A same-named record registered by any other extension is
    /// not taken, so the claim check still sees and refuses that clash
    /// instead of the prune hiding it.
    fn took_from(&self, kind: ReplaceKind, record: &Name, owner: &str) -> bool {
        self.of(kind)
            .get(record)
            .is_some_and(|holder| holder.owner.as_str() == owner)
    }
}

/// Settles one declared batch: the per-record replacement contract.
///
/// Claims evaluate in canonical replacer order, so two replacements of one
/// record refuse deterministically: the bytewise-first plugin settles and
/// the next one names it. A taken record leaves the bundled extension that
/// owns it — and that extension alone, and only that record — by filtering
/// its declarations in place, so the surviving records keep their declared
/// order, an untouched extension is returned untouched, and the batch keeps
/// its order and length. The pruned batch then passes the same canonical
/// claim check a generation publish runs, which keeps every remaining
/// bare-name clash a load error naming both records. Without any claim the
/// batch comes back as declared.
///
/// # Errors
///
/// Returns the typed [`RegistrationError`] for the first refused claim or
/// the first remaining record clash, together with the batch, boxed because
/// the pair is large; the caller renders the loader location it owns.
pub fn settle(extensions: Vec<Extension>) -> Result<Vec<Extension>, Box<Refused>> {
    let taken = match settle_claims(&extensions) {
        Ok(taken) => taken,
        Err(error) => return Err(Box::new(Refused { error, extensions })),
    };
    let pruned = prune(extensions, &taken);
    match check_claims(&pruned) {
        Ok(()) => Ok(pruned),
        Err(error) => Err(Box::new(Refused {
            error,
            extensions: pruned,
        })),
    }
}

/// Validates every replacement claim in canonical replacer order and
/// returns the ledger of taken records.
fn settle_claims(batch: &[Extension]) -> Result<Taken, RegistrationError> {
    let mut replacers: Vec<&Extension> = batch
        .iter()
        .filter(|ext| !ext.replace_claims().is_empty())
        .collect();
    replacers.sort_by(|a, b| canonical_order(a, b));
    let mut taken = Taken::default();
    for replacer in replacers {
        let replacer_name = Name::parse(replacer.name())?;
        for claim in replacer.replace_claims() {
            judge(batch, replacer, &replacer_name, claim, &taken)?;
            taken.of_mut(claim.kind).insert(
                claim.record.clone(),
                Holder {
                    owner: claim.owner.clone(),
                    replacer: replacer_name.clone(),
                },
            );
        }
    }
    Ok(taken)
}

/// Judges one replacement claim against the declared batch.
///
/// The claimer class comes first — only a user plugin replaces — then the
/// owner class — only a bundled extension is a battery — then the record
/// ownership, and last the settlement order: a record another claim already
/// took refuses naming the first holder.
fn judge(
    batch: &[Extension],
    replacer: &Extension,
    replacer_name: &Name,
    claim: &ReplaceClaim,
    taken: &Taken,
) -> Result<(), RegistrationError> {
    let refuse = |reason| RegistrationError::CannotReplace {
        kind: claim.kind,
        record: claim.record.clone(),
        replacer: replacer_name.clone(),
        site: declared_site(replacer),
        reason,
    };
    if replacer.origin() != Origin::User {
        return Err(refuse(ReplaceRefusal::ClaimerNotUser));
    }
    let Some(owner) = batch.iter().find(|ext| ext.name() == claim.owner.as_str()) else {
        return Err(refuse(ReplaceRefusal::NoBattery {
            owner: claim.owner.clone(),
        }));
    };
    match owner.origin() {
        Origin::Bundled => {}
        Origin::Builtin => {
            return Err(refuse(ReplaceRefusal::OwnerBuiltin {
                owner: claim.owner.clone(),
            }));
        }
        Origin::User => {
            return Err(refuse(ReplaceRefusal::OwnerUser {
                owner: claim.owner.clone(),
            }));
        }
        // A future origin class is no bundled battery either.
        _ => {
            return Err(refuse(ReplaceRefusal::NoBattery {
                owner: claim.owner.clone(),
            }));
        }
    }
    if !registers(owner, claim) {
        let actual = batch
            .iter()
            .filter(|ext| !std::ptr::eq(*ext, replacer) && registers(ext, claim))
            .min_by(|a, b| canonical_order(a, b));
        return Err(refuse(match actual {
            None => ReplaceRefusal::NotOwned {
                owner: claim.owner.clone(),
            },
            Some(actual) => ReplaceRefusal::NotOwnedElsewhere {
                owner: claim.owner.clone(),
                actual: claimant_of(actual)?.0,
            },
        }));
    }
    if let Some(holder) = taken.of(claim.kind).get(&claim.record) {
        return Err(refuse(ReplaceRefusal::AlreadyReplaced {
            holder: holder.replacer.clone(),
        }));
    }
    Ok(())
}

/// Whether `ext` registers the record `claim` names.
fn registers(ext: &Extension, claim: &ReplaceClaim) -> bool {
    match claim.kind {
        ReplaceKind::Skill => ext.skills().iter().any(|skill| skill.name == claim.record),
        ReplaceKind::Rule => ext.rules().iter().any(|rule| rule.name == claim.record),
    }
}

/// Removes every taken record from the bundled extension that owned it,
/// keeping the declared order of everything that survives.
fn prune(extensions: Vec<Extension>, taken: &Taken) -> Vec<Extension> {
    if taken.is_empty() {
        return extensions;
    }
    extensions
        .into_iter()
        .map(|mut ext| {
            if ext.origin() == Origin::Bundled {
                let owner: &str = &ext.name;
                ext.skills
                    .retain(|skill| !taken.took_from(ReplaceKind::Skill, &skill.name, owner));
                ext.rules
                    .retain(|rule| !taken.took_from(ReplaceKind::Rule, &rule.name, owner));
            }
            ext
        })
        .collect()
}

/// Runs the canonical claim check over the pruned batch: the same ledger a
/// generation publish runs, in the same canonical order, so every remaining
/// bare-name clash — including the default clash a removed replacement
/// brings back — fails here at load naming both records.
fn check_claims(extensions: &[Extension]) -> Result<(), RegistrationError> {
    let mut order: Vec<&Extension> = extensions.iter().collect();
    order.sort_by(|a, b| canonical_order(a, b));
    let mut tables = ClaimTables::new();
    for ext in order {
        tables.check(ext)?;
    }
    Ok(())
}

/// Where the extension declared itself. A user plugin converted by the
/// loader always carries its `plugin.star` site; an extension without one
/// reports its name at position zero, because the refusal needs a site and
/// the extension has no file.
fn declared_site(ext: &Extension) -> Site {
    ext.site().cloned().unwrap_or_else(|| Site {
        path: std::path::PathBuf::from(ext.name()),
        line: 0,
        col: 0,
    })
}

#[cfg(test)]
mod tests {
    use dal_core::{Claimant, RuleRecord, ServiceSet, SkillRecord};

    use super::*;
    use crate::ext::ExtensionBuilder;

    type Failure = Box<dyn std::error::Error>;
    type Outcome = Result<(), Failure>;
    type Built = Result<Extension, RegistrationError>;

    fn skill(name: &str) -> Result<SkillRecord, RegistrationError> {
        Ok(SkillRecord {
            name: Name::parse(name)?,
            description: "d".into(),
            body: "body".into(),
            letter2image: false,
            mcp: None,
        })
    }

    fn rule(name: &str) -> Result<RuleRecord, RegistrationError> {
        Ok(RuleRecord {
            name: Name::parse(name)?,
            patterns: vec!["needle".into()],
            text: "reminder".into(),
            judge: None,
            scope: None,
            globs: None,
            agents: None,
            mode: None,
            repeat_mode: None,
            repeat_gap: None,
            always_apply: false,
            report: false,
            enabled: true,
        })
    }

    fn builder(name: &str, origin: Origin) -> Result<ExtensionBuilder, RegistrationError> {
        Ok(ExtensionBuilder::new(name, "0.1.0", ServiceSet::EMPTY)?.with_origin(origin, None))
    }

    /// A bundled battery registering the given skills, then the given rules.
    fn battery(name: &str, skills: &[&str], rules: &[&str]) -> Built {
        let mut registered = builder(name, Origin::Bundled)?;
        for record in skills {
            registered = registered.skill(skill(record)?);
        }
        for record in rules {
            registered = registered.rule(rule(record)?);
        }
        registered.build()
    }

    /// A user plugin registering one skill without any claim.
    fn plain(name: &str, record: &str) -> Built {
        builder(name, Origin::User)?.skill(skill(record)?).build()
    }

    /// A user plugin whose one skill replaces `owner`'s record.
    fn replacing_skill(name: &str, record: &str, owner: &str) -> Built {
        builder(name, Origin::User)?
            .skill_replacing(skill(record)?, Name::parse(owner)?)
            .build()
    }

    /// A user plugin whose one rule replaces `owner`'s record.
    fn replacing_rule(name: &str, record: &str, owner: &str) -> Built {
        builder(name, Origin::User)?
            .rule_replacing(rule(record)?, Name::parse(owner)?)
            .build()
    }

    fn find<'a>(batch: &'a [Extension], name: &str) -> Result<&'a Extension, Failure> {
        batch
            .iter()
            .find(|ext| ext.name() == name)
            .ok_or_else(|| format!("no extension \"{name}\" in the batch").into())
    }

    fn skill_names(ext: &Extension) -> Vec<&str> {
        ext.skills()
            .iter()
            .map(|record| record.name.as_str())
            .collect()
    }

    fn rule_names(ext: &Extension) -> Vec<&str> {
        ext.rules()
            .iter()
            .map(|record| record.name.as_str())
            .collect()
    }

    /// The typed refusal a batch must settle into.
    fn refusal(batch: Vec<Extension>) -> Result<RegistrationError, Failure> {
        match settle(batch) {
            Ok(_) => Err("the batch settled".into()),
            Err(refused) => {
                let Refused { error, .. } = *refused;
                Ok(error)
            }
        }
    }

    /// The replacement refusal reason a batch must settle into.
    fn refused_reason(batch: Vec<Extension>) -> Result<ReplaceRefusal, Failure> {
        match refusal(batch)? {
            RegistrationError::CannotReplace { reason, .. } => Ok(reason),
            error => Err(error.into()),
        }
    }

    #[test]
    fn a_settled_claim_prunes_only_the_owner_record_in_order() -> Outcome {
        let batch = vec![
            battery("skills", &["a", "b", "c"], &["b"])?,
            battery("more", &["second"], &[])?,
            replacing_skill("mine", "b", "skills")?,
        ];
        let settled = settle(batch).map_err(|refused| refused.error)?;
        let names: Vec<&str> = settled.iter().map(Extension::name).collect();
        assert_eq!(names, ["skills", "more", "mine"]);
        let owner = find(&settled, "skills")?;
        assert_eq!(skill_names(owner), ["a", "c"]);
        assert_eq!(rule_names(owner), ["b"]);
        assert_eq!(skill_names(find(&settled, "more")?), ["second"]);
        assert_eq!(skill_names(find(&settled, "mine")?), ["b"]);
        Ok(())
    }

    #[test]
    fn a_rule_claim_takes_the_rule_and_leaves_the_same_named_skill() -> Outcome {
        let batch = vec![
            battery("rules", &["shared"], &["shared", "kept"])?,
            replacing_rule("mine", "shared", "rules")?,
        ];
        let settled = settle(batch).map_err(|refused| refused.error)?;
        let owner = find(&settled, "rules")?;
        assert_eq!(skill_names(owner), ["shared"]);
        assert_eq!(rule_names(owner), ["kept"]);
        assert_eq!(rule_names(find(&settled, "mine")?), ["shared"]);
        Ok(())
    }

    #[test]
    fn two_claims_from_one_plugin_keep_the_surviving_order() -> Outcome {
        let mine = builder("mine", Origin::User)?
            .skill_replacing(skill("a")?, Name::parse("skills")?)
            .skill_replacing(skill("c")?, Name::parse("skills")?)
            .build()?;
        let settled = settle(vec![battery("skills", &["a", "b", "c", "d"], &[])?, mine])
            .map_err(|refused| refused.error)?;
        assert_eq!(skill_names(find(&settled, "skills")?), ["b", "d"]);
        assert_eq!(skill_names(find(&settled, "mine")?), ["a", "c"]);
        Ok(())
    }

    #[test]
    fn a_batch_without_claims_settles_as_declared() -> Outcome {
        let batch = vec![
            battery("skills", &["a", "b"], &["r"])?,
            plain("mine", "own")?,
        ];
        let settled = settle(batch).map_err(|refused| refused.error)?;
        assert_eq!(skill_names(find(&settled, "skills")?), ["a", "b"]);
        assert_eq!(rule_names(find(&settled, "skills")?), ["r"]);
        assert_eq!(skill_names(find(&settled, "mine")?), ["own"]);
        Ok(())
    }

    #[test]
    fn a_bare_clash_without_a_claim_still_refuses() -> Outcome {
        let batch = vec![battery("skills", &["a"], &[])?, plain("mine", "a")?];
        match refusal(batch)? {
            RegistrationError::Conflict {
                kind,
                claimant,
                rival,
                ..
            } => {
                assert_eq!(kind, "skill");
                assert_eq!(claimant, Claimant::Plugin(Name::parse("skills")?));
                assert_eq!(rival, Claimant::Plugin(Name::parse("mine")?));
                Ok(())
            }
            error => Err(error.into()),
        }
    }

    #[test]
    fn a_replaced_record_still_clashes_with_a_third_extension_registering_it() -> Outcome {
        let batch = vec![
            battery("skills", &["a"], &[])?,
            plain("other", "a")?,
            replacing_skill("mine", "a", "skills")?,
        ];
        match refusal(batch)? {
            RegistrationError::Conflict {
                claimant, rival, ..
            } => {
                assert_eq!(claimant, Claimant::Plugin(Name::parse("mine")?));
                assert_eq!(rival, Claimant::Plugin(Name::parse("other")?));
                Ok(())
            }
            error => Err(error.into()),
        }
    }

    #[test]
    fn an_absent_owner_refuses_naming_the_expected_battery() -> Outcome {
        let batch = vec![replacing_skill("mine", "a", "absent")?];
        assert_eq!(
            refused_reason(batch)?,
            ReplaceRefusal::NoBattery {
                owner: Name::parse("absent")?
            }
        );
        Ok(())
    }

    #[test]
    fn a_builtin_owner_refuses() -> Outcome {
        let builtin = builder("tools", Origin::Builtin)?
            .skill(skill("a")?)
            .build()?;
        let batch = vec![builtin, replacing_skill("mine", "a", "tools")?];
        assert_eq!(
            refused_reason(batch)?,
            ReplaceRefusal::OwnerBuiltin {
                owner: Name::parse("tools")?
            }
        );
        Ok(())
    }

    #[test]
    fn a_user_owner_refuses() -> Outcome {
        let batch = vec![plain("other", "a")?, replacing_skill("mine", "a", "other")?];
        assert_eq!(
            refused_reason(batch)?,
            ReplaceRefusal::OwnerUser {
                owner: Name::parse("other")?
            }
        );
        Ok(())
    }

    #[test]
    fn a_plugin_cannot_name_itself_as_the_owner() -> Outcome {
        let batch = vec![replacing_skill("mine", "a", "mine")?];
        assert_eq!(
            refused_reason(batch)?,
            ReplaceRefusal::OwnerUser {
                owner: Name::parse("mine")?
            }
        );
        Ok(())
    }

    #[test]
    fn an_owner_without_the_record_refuses_when_nobody_else_has_it() -> Outcome {
        let batch = vec![
            battery("skills", &["kept"], &[])?,
            replacing_skill("mine", "a", "skills")?,
        ];
        assert_eq!(
            refused_reason(batch)?,
            ReplaceRefusal::NotOwned {
                owner: Name::parse("skills")?
            }
        );
        Ok(())
    }

    #[test]
    fn an_owner_without_the_record_names_the_extension_that_has_it() -> Outcome {
        let batch = vec![
            replacing_skill("mine", "a", "skills")?,
            battery("other-battery", &["a"], &[])?,
            battery("skills", &["kept"], &[])?,
        ];
        assert_eq!(
            refused_reason(batch)?,
            ReplaceRefusal::NotOwnedElsewhere {
                owner: Name::parse("skills")?,
                actual: Claimant::Plugin(Name::parse("other-battery")?),
            }
        );
        Ok(())
    }

    #[test]
    fn a_claim_checks_the_record_kind_of_the_owner() -> Outcome {
        let batch = vec![
            battery("skills", &["a"], &[])?,
            replacing_rule("mine", "a", "skills")?,
        ];
        assert_eq!(
            refused_reason(batch)?,
            ReplaceRefusal::NotOwned {
                owner: Name::parse("skills")?
            }
        );
        Ok(())
    }

    #[test]
    fn the_second_replacement_in_name_order_refuses_regardless_of_input_order() -> Outcome {
        let batch = vec![
            battery("skills", &["a"], &[])?,
            replacing_skill("beta", "a", "skills")?,
            replacing_skill("alpha", "a", "skills")?,
        ];
        match refusal(batch)? {
            RegistrationError::CannotReplace {
                replacer, reason, ..
            } => {
                assert_eq!(replacer.as_str(), "beta");
                assert_eq!(
                    reason,
                    ReplaceRefusal::AlreadyReplaced {
                        holder: Name::parse("alpha")?
                    }
                );
                Ok(())
            }
            error => Err(error.into()),
        }
    }

    #[test]
    fn a_builtin_or_bundled_claimer_refuses() -> Outcome {
        for origin in [Origin::Builtin, Origin::Bundled] {
            let claimer = builder("tools", origin)?
                .skill_replacing(skill("a")?, Name::parse("skills")?)
                .build()?;
            let batch = vec![battery("skills", &["a"], &[])?, claimer];
            assert_eq!(refused_reason(batch)?, ReplaceRefusal::ClaimerNotUser);
        }
        Ok(())
    }

    #[test]
    fn a_refusal_points_at_the_declared_claim_site() -> Outcome {
        let site = Site {
            path: "plugin.star".into(),
            line: 7,
            col: 3,
        };
        let mine = ExtensionBuilder::new("mine", "0.1.0", ServiceSet::EMPTY)?
            .with_origin(Origin::User, Some(site.clone()))
            .skill_replacing(skill("a")?, Name::parse("absent")?)
            .build()?;
        match refusal(vec![mine])? {
            RegistrationError::CannotReplace {
                kind,
                record,
                replacer,
                site: reported,
                ..
            } => {
                assert_eq!(kind, ReplaceKind::Skill);
                assert_eq!(record.as_str(), "a");
                assert_eq!(replacer.as_str(), "mine");
                assert_eq!(reported, site);
                Ok(())
            }
            error => Err(error.into()),
        }
    }
}
