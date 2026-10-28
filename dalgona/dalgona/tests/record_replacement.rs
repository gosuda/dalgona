// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Explicit per-record replacement of one bundled battery record by a user
//! plugin.
//!
//! The SDK contract under test: `dal.skill(...)` and `dal.rule(...)`
//! take `replace = True` together with a required explicit owner
//! `battery = "<bundled extension name>"`. `replace = False` or no flag at
//! all keeps the default refusal. `battery` without `replace = True`, and
//! `replace = True` without `battery`, are strict input refusals. The
//! extension name itself is never replaceable.
//!
//! Every test drives the complete dalgona assembly (`dalgona::build`, and
//! `Host::start` where the claim tables or the published documents decide)
//! over a real `<data_root>/plugins/<name>/plugin.star` that the real
//! Starlark loader reads. No metadata is copied by hand: assertions read the
//! extension records, grant keys, and `skill://` documents the product made.
//!
//! The two `a_user_*_named_like_*` controls carry no flag: they pin the
//! default refusal through the real `skills` battery, the real `quality`
//! battery, and the real claim tables. The wrong-type cases write `replace`
//! (a `bool`) and `battery` (a `str`) with values of another type: the real
//! loader refuses each at the declaring call as a type rejection of that
//! keyword, never as an unknown keyword. The refusal is the typed `plugins`
//! section failure of the product build; its cause is the public plugin-load
//! evaluation failure that points at the real `plugin.star` and names the
//! keyword and the type it takes. A refused load leaves the plugin file as it
//! found it.
//!
//! The reload cases open a real session over the same assembly and drive the
//! real `/reload` command (`Command::ReloadPlugins` through `Agent::submit`)
//! over plugin files that the test edits on disk between reloads. Their oracle
//! is the live host: the published generation id and the `skill://` documents
//! `Host::doc` serves, never the extension records or an identity alone. A
//! refusal is asserted by the record and the category it names, not by its
//! whole wording.
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dal_agent::ext::Extension;
use dal_agent::ext::grants::GrantKey;
use dal_agent::{Agent, AgentError, Host, Product, SessionRef};
use dal_core::{ClientId, Command, Config, ConfigProduct, Origin, ServiceSet, Workspace};
use dalgon::BuildCx;
use dalgona_batteries::skills::{
    DELEGATE_WITH_CONTRACTS_SKILL, FIND_ANYTHING_SKILL, INITIALIZER_AND_SPRINTS_SKILL,
};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

/// The bundled `skills` battery's skill that the replacing plugin takes.
const BATTERY_SKILL: &str = "find-anything";
/// The bundled `skills` battery extension, the explicit owner of the skill.
const OWNER_SKILLS: &str = "skills";
/// The bundled `quality` battery's detector rule record. The quality battery
/// also owns services, a status kind, tools, and a prompt section, so this
/// record shows what a replacement must not inherit.
const BATTERY_RULE: &str = "fabricated-unavailable-tool-call";
/// The bundled `quality` battery extension, the explicit owner of the rule.
const OWNER_QUALITY: &str = "quality";
/// The body of the replacing plugin's `SKILL.md`.
const SKILL_BODY: &str = "# Mine\n\nThe replacement body is unique to this test.\n";
/// A skill name that no bundled battery registers.
const FRESH_SKILL: &str = "fresh-skill";
/// The skill of the unrelated user plugin `other`.
const OTHER_SKILL: &str = "other-skill";
/// The body of `other`'s `SKILL.md`. It differs from [`SKILL_BODY`], so a
/// served body names the plugin that owns it.
const OTHER_BODY: &str = "# Other\n\nThe unrelated plugin owns this body.\n";
/// A body that reaches the disk only through an edit between two reloads, so
/// a reload that serves it re-read the file.
const EDITED_BODY: &str = "# Edited\n\nThis body exists only after the edit.\n";
/// The body of `FIRST.md` in the two-skill plugin.
const FIRST_BODY: &str = "# First\n";
/// The body of `SECOND.md` in the two-skill plugin.
const SECOND_BODY: &str = "# Second\n";

struct Root(PathBuf);

impl Root {
    fn create() -> io::Result<Self> {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "dalgona-replacement-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One user plugin written under `<data_root>/plugins/<name>/`.
struct Plugin {
    name: &'static str,
    source: String,
    files: &'static [(&'static str, &'static str)],
}

/// The replacement claim one record constructor carries.
enum Claim {
    /// No flag at all: the default refusal that holds today.
    None,
    /// `replace = False` alone: the explicit default, same refusal.
    Defaulted,
    /// `replace = True` with the required explicit bundled owner.
    Owner(&'static str),
    /// `battery` without `replace = True`: strict input refusal.
    OwnerWithoutReplace(&'static str),
    /// `replace = True` without `battery`: strict input refusal.
    ReplaceWithoutOwner,
    /// `replace` written with `literal`, a value that is not a `bool`, next
    /// to the well-formed bundled `owner`.
    WrongFlag {
        literal: &'static str,
        owner: &'static str,
    },
    /// `battery` written with `literal`, a value that is not a string, next
    /// to `replace = True`.
    WrongOwner { literal: &'static str },
}

impl Claim {
    /// The keyword a wrong-typed claim writes, and the literal it writes.
    fn wrong_typed(&self) -> Option<(Keyword, &'static str)> {
        match self {
            Self::WrongFlag { literal, .. } => Some((Keyword::Replace, *literal)),
            Self::WrongOwner { literal } => Some((Keyword::Battery, *literal)),
            _ => None,
        }
    }
}

/// A keyword of the replacement pair.
#[derive(Clone, Copy)]
enum Keyword {
    Replace,
    Battery,
}

impl Keyword {
    fn name(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Battery => "battery",
        }
    }

    /// The type the settled contract gives the keyword: a flag is a `bool`,
    /// an owner a string. `str` is how the Starlark library spells the
    /// string type when a diagnostic says what it expected.
    fn expected(self) -> &'static str {
        match self {
            Self::Replace => "bool",
            Self::Battery => "str",
        }
    }
}

fn claim_kwargs(claim: &Claim) -> String {
    match claim {
        Claim::None => String::new(),
        Claim::Defaulted => ", replace = False".to_owned(),
        Claim::Owner(owner) => format!(", replace = True, battery = {owner:?}"),
        Claim::OwnerWithoutReplace(owner) => format!(", battery = {owner:?}"),
        Claim::ReplaceWithoutOwner => ", replace = True".to_owned(),
        Claim::WrongFlag { literal, owner } => {
            format!(", replace = {literal}, battery = {owner:?}")
        }
        Claim::WrongOwner { literal } => format!(", replace = True, battery = {literal}"),
    }
}

/// A plugin `name` that registers one skill `skill` carrying `claim`.
fn skill_plugin(name: &'static str, skill: &str, claim: &Claim) -> Plugin {
    let source = format!(
        "load(\"@dal/v1\", \"dal\")\n\nmine = dal.skill(description = \"Mine\", path = \"SKILL.md\"{})\n\nplugin = dal.plugin(name = {name:?}, version = \"0.1.0\", skills = {{{skill:?}: mine}})\n",
        claim_kwargs(claim),
    );
    Plugin {
        name,
        source,
        files: &[("SKILL.md", SKILL_BODY)],
    }
}

/// A plugin `mine` that registers two skills, `a` and `b`, each carrying its
/// own claim. `a` takes the body of `FIRST.md`, `b` the body of `SECOND.md`.
fn two_skill_plugin(a: &str, claim_a: &Claim, b: &str, claim_b: &Claim) -> Plugin {
    let source = format!(
        "load(\"@dal/v1\", \"dal\")\n\nfirst = dal.skill(description = \"First\", path = \"FIRST.md\"{})\nsecond = dal.skill(description = \"Second\", path = \"SECOND.md\"{})\n\nplugin = dal.plugin(name = \"mine\", version = \"0.1.0\", skills = {{{a:?}: first, {b:?}: second}})\n",
        claim_kwargs(claim_a),
        claim_kwargs(claim_b),
    );
    Plugin {
        name: "mine",
        source,
        files: &[("FIRST.md", FIRST_BODY), ("SECOND.md", SECOND_BODY)],
    }
}

/// `plugin` with `files` beside its `plugin.star`: the same declarations over
/// other file bodies.
fn with_files(plugin: Plugin, files: &'static [(&'static str, &'static str)]) -> Plugin {
    Plugin { files, ..plugin }
}

/// The user plugin `other`: one unclaimed skill of its own, unrelated to any
/// battery record.
fn other_plugin() -> Plugin {
    with_files(
        skill_plugin("other", OTHER_SKILL, &Claim::None),
        &[("SKILL.md", OTHER_BODY)],
    )
}

/// A plugin `name` that registers one rule `rule` carrying `claim`.
fn rule_plugin(name: &'static str, rule: &str, claim: &Claim) -> Plugin {
    let source = format!(
        "load(\"@dal/v1\", \"dal\")\n\nmine = dal.rule(pattern = \"zz-never-matches\", text = \"Mine\"{})\n\nplugin = dal.plugin(name = {name:?}, version = \"0.1.0\", rules = {{{rule:?}: mine}})\n",
        claim_kwargs(claim),
    );
    Plugin {
        name,
        source,
        files: &[],
    }
}

fn write_plugins(root: &Root, plugins: &[Plugin]) -> io::Result<()> {
    for plugin in plugins {
        let directory = root.0.join("plugins").join(plugin.name);
        fs::create_dir_all(&directory)?;
        fs::write(directory.join("plugin.star"), &plugin.source)?;
        for (file, text) in plugin.files {
            fs::write(directory.join(file), text)?;
        }
    }
    Ok(())
}

/// Writes the plugins, then loads the layered dalgona config that lists them.
fn prepared(plugins: &[Plugin]) -> Result<(Root, Config), Box<dyn Error>> {
    let root = Root::create()?;
    write_plugins(&root, plugins)?;
    let listed: Vec<String> = plugins
        .iter()
        .map(|plugin| format!("{:?}", plugin.name))
        .collect();
    let toml = format!("plugins = [{}]\n", listed.join(", "));
    let factory = dalgona::product();
    let config = Config::load(
        ConfigProduct::Dalgona,
        &root.0,
        factory.defaults,
        Some(toml.as_str()),
    )?;
    Ok((root, config))
}

fn product_of(root: &Root, config: &Config) -> Result<Product, Box<dyn Error>> {
    let cx = BuildCx {
        data_root: root.0.clone(),
        config,
    };
    Ok(dalgona::build(&cx)?)
}

fn built(plugins: &[Plugin]) -> Result<Product, Box<dyn Error>> {
    let (root, config) = prepared(plugins)?;
    product_of(&root, &config)
}

/// The text of a build that must fail.
fn failure(plugins: &[Plugin]) -> Result<String, Box<dyn Error>> {
    match built(plugins) {
        Ok(_) => Err(io::Error::other("the build accepted the plugins").into()),
        Err(error) => Ok(error.to_string()),
    }
}

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

/// What a test body left behind: its result, or the panic it raised.
type Outcome = std::thread::Result<Result<(), Box<dyn Error>>>;

/// Shuts the host down with a bounded grace, reads its report, and only then
/// returns what the test body left. The plugin root and the runtime stay owned
/// by the caller, so both outlive the shutdown.
///
/// `sessions` is the number of sessions the body had open. A session left
/// open, a status kind that never went quiet, or a task left behind fails an
/// otherwise passing case. When the body already failed, its error or panic
/// payload is returned or re-raised unchanged after the shutdown, and an
/// unclean shutdown is printed beside it, never in its place.
fn shut_down(
    runtime: &tokio::runtime::Runtime,
    host: Host,
    sessions: usize,
    outcome: Outcome,
) -> Result<(), Box<dyn Error>> {
    let report = runtime.block_on(host.shutdown(Duration::from_secs(2)));
    let left = format!(
        "{} of {sessions} sessions closed, status quiet: {}, {} tasks remaining",
        report.sessions_closed, report.status_quiet, report.tasks_remaining
    );
    let clean =
        report.sessions_closed == sessions && report.status_quiet && report.tasks_remaining == 0;
    if !clean && !matches!(&outcome, Ok(Ok(()))) {
        eprintln!("the shutdown was not clean either: {left}");
    }
    match outcome {
        Ok(Ok(())) if clean => Ok(()),
        Ok(Ok(())) => Err(io::Error::other(format!("the shutdown was not clean: {left}")).into()),
        Ok(Err(error)) => Err(error),
        Err(payload) => resume_unwind(payload),
    }
}

/// Starts the real host over the built product and runs `run` against it.
///
/// Only `run` is inside the panic boundary: the host is always shut down and
/// its report read before the result or the panic of `run` leaves this function.
fn with_host(
    plugins: &[Plugin],
    run: impl FnOnce(&Host) -> Result<(), Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let (root, config) = prepared(plugins)?;
    let product = product_of(&root, &config)?;
    let env = dal_agent::Env::data_root(root.0.clone());
    let runtime = runtime()?;
    let host = runtime.block_on(Host::start(product, config, env))?;
    let outcome = catch_unwind(AssertUnwindSafe(|| runtime.block_on(async { run(&host) })));
    shut_down(&runtime, host, 0, outcome)
}

/// The text of a `Host::start` that must fail after a clean build.
fn host_start_failure(plugins: &[Plugin]) -> Result<String, Box<dyn Error>> {
    match with_host(plugins, |_| Ok(())) {
        Ok(()) => Err(io::Error::other("the host started over the clashing records").into()),
        Err(error) => Ok(error.to_string()),
    }
}

/// Opens a real session over the built product and runs `run` with the host
/// and the session agent: the reload cases drive the real `/reload` command
/// through `Agent::submit` over plugin files edited under the live data root.
///
/// `run` is an async closure, so its future may hold `&Host` and `&Agent`
/// across awaits. Only `run` is inside the panic boundary: the host is started
/// and the session opened on the same runtime beforehand, and the host is
/// always shut down, a failed open included, and its report read before the
/// result or the panic of `run` leaves this function.
fn with_session(
    plugins: &[Plugin],
    run: impl AsyncFnOnce(&Host, &Agent) -> Result<(), Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let (root, config) = prepared(plugins)?;
    let product = product_of(&root, &config)?;
    let env = dal_agent::Env::data_root(root.0.clone());
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&workspace)?;
    let workspace = Workspace::new(workspace)?;
    let runtime = runtime()?;
    let host = runtime.block_on(Host::start(product, config, env))?;
    let session = SessionRef::Ephemeral { workspace };
    let opened = runtime.block_on(host.open(session, ClientId::new("record-replacement")));
    let (outcome, sessions) = match opened {
        Ok(agent) => {
            let outcome = catch_unwind(AssertUnwindSafe(|| runtime.block_on(run(&host, &agent))));
            (outcome, 1)
        }
        Err(error) => (Ok(Err(error.into())), 0),
    };
    shut_down(&runtime, host, sessions, outcome)
}

/// Overwrites `plugin` under the live data root, as an author who edits the
/// plugin files on disk between two reloads.
fn edit_plugin(host: &Host, plugin: &Plugin) -> io::Result<()> {
    let directory = host.data_root().join("plugins").join(plugin.name);
    fs::write(directory.join("plugin.star"), &plugin.source)?;
    for (file, text) in plugin.files {
        fs::write(directory.join(file), text)?;
    }
    Ok(())
}

/// The text of a `/reload` that the product must refuse. The refusal is the
/// validation failure of the command (`AgentError::Invalid`); a closed session
/// or any other failure is a broken harness, not a refused plugin set.
async fn reload_failure(agent: &Agent) -> Result<String, Box<dyn Error>> {
    match agent.submit(Command::ReloadPlugins).await {
        Ok(_) => Err(io::Error::other("the reload accepted the plugins").into()),
        Err(AgentError::Invalid(refusal)) => Ok(refusal.message().to_owned()),
        Err(other) => Err(io::Error::other(format!(
            "the reload failed without refusing the plugins: {other}"
        ))
        .into()),
    }
}

/// Submits a `/reload` that must succeed and publish.
async fn reloaded(agent: &Agent) -> Result<(), Box<dyn Error>> {
    let _reply = agent.submit(Command::ReloadPlugins).await?;
    Ok(())
}

fn names(product: &Product) -> BTreeSet<&str> {
    product.extensions.iter().map(Extension::name).collect()
}

fn extension<'a>(product: &'a Product, name: &str) -> Result<&'a Extension, Box<dyn Error>> {
    product
        .extensions
        .iter()
        .find(|extension| extension.name() == name)
        .ok_or_else(|| io::Error::other(format!("the product has no `{name}` extension")).into())
}

fn skill_set(extension: &Extension) -> BTreeSet<&str> {
    extension
        .skills()
        .iter()
        .map(|skill| skill.name.as_str())
        .collect()
}

fn rule_set(extension: &Extension) -> BTreeSet<&str> {
    extension
        .rules()
        .iter()
        .map(|rule| rule.name.as_str())
        .collect()
}

fn assert_mentions(text: &str, fragments: &[&str]) {
    for fragment in fragments {
        assert!(text.contains(fragment), "missing {fragment:?} in {text:?}");
    }
}

/// Asserts that `host` serves `body` at `uri`: the published document a client
/// reads, not the record an extension registered.
fn assert_served(host: &Host, uri: &str, body: &str) -> Result<(), Box<dyn Error>> {
    let doc = host.doc(uri)?;
    assert_eq!(&*doc.text, body, "{uri}");
    Ok(())
}

/// Asserts that `host` serves the three skills of the bundled `skills`
/// battery, with `find_anything` as the body of `find-anything` (the skill the
/// reload cases replace) and the bundled body for the other two.
fn assert_battery_skills(host: &Host, find_anything: &str) -> Result<(), Box<dyn Error>> {
    assert_served(host, "skill://find-anything", find_anything)?;
    assert_served(
        host,
        "skill://delegate-with-contracts",
        DELEGATE_WITH_CONTRACTS_SKILL,
    )?;
    assert_served(
        host,
        "skill://initializer-and-sprints",
        INITIALIZER_AND_SPRINTS_SKILL,
    )
}

/// The one-based line of the declaring call in a fixture plugin source: the
/// line that binds the record constructor as `mine = dal.…(`.
fn declaring_line(source: &str) -> Result<usize, Box<dyn Error>> {
    source
        .lines()
        .position(|line| line.starts_with("mine = dal."))
        .map(|index| index + 1)
        .ok_or_else(|| io::Error::other("the plugin source declares no `mine`").into())
}

/// Loads `plugin`, whose `claim` writes a value of another type for one
/// keyword of the pair. The build must refuse it as the typed `plugins`
/// section failure whose cause is the public plugin-load evaluation failure
/// ([`dal_star::LoadError::Eval`]) at the real `plugin.star`. The evaluator
/// text must have one line that names the quoted keyword and says what it
/// expected, quoting the type the keyword takes; it must point at the line
/// of the declaring call; and it must not be the refusal of an unknown
/// keyword: that one quotes the keyword but states no expectation, so a build
/// that does not declare the keyword cannot pass here. The echoed call line
/// and the data root name carry the bare word, never the quoted keyword or
/// the quoted expectation. The refused load must leave the plugin file as it
/// found it.
///
/// The loader reports `Eval` at line 1, column 1 (`load_errors.rs` in
/// `dal-star` pins that the vendor text carries the real location), so the
/// structured line and column carry no information and are not read; the
/// column of the call is the evaluator's rendering and is not asserted.
fn assert_wrong_type(plugin: &Plugin, claim: &Claim) -> Result<(), Box<dyn Error>> {
    let Some((keyword, literal)) = claim.wrong_typed() else {
        return Err(io::Error::other("the claim is not a wrong-typed one").into());
    };
    let label = format!("{} = {literal}", keyword.name());
    let (root, config) = prepared(std::slice::from_ref(plugin))?;
    let star = root.0.join("plugins").join(plugin.name).join("plugin.star");
    let written = fs::read(&star)?;
    let line = declaring_line(std::str::from_utf8(&written)?)?;

    let Err(error) = product_of(&root, &config) else {
        return Err(io::Error::other(format!("the build accepted {label}")).into());
    };
    let error = error
        .downcast::<dalgon::BuildError>()
        .map_err(|other| io::Error::other(format!("{label}: not a build error: {other}")))?;
    let dalgon::BuildError::Section { section, source } = &*error else {
        return Err(io::Error::other(format!("{label}: not a section failure: {error}")).into());
    };
    assert_eq!(&**section, "plugins", "{label}");
    let Some(dal_star::LoadError::Eval { path, message, .. }) =
        source.downcast_ref::<dal_star::LoadError>()
    else {
        return Err(io::Error::other(format!(
            "{label}: not the plugin-load evaluation failure: {source}"
        ))
        .into());
    };
    assert_eq!(
        fs::canonicalize(path)?,
        fs::canonicalize(&star)?,
        "{label}: the failure points at another file"
    );

    let named = format!("`{}`", keyword.name());
    let expectation = format!("expected `{}`", keyword.expected());
    assert!(
        message
            .lines()
            .any(|text| text.contains(&named) && text.contains(&expectation)),
        "{label}: no line names {named} and says {expectation}: {message}"
    );
    let site = format!("plugin.star:{line}");
    assert_mentions(message, &[site.as_str()]);
    assert!(
        !message.contains("extra named"),
        "{label}: refused as an unknown keyword, not as a type mismatch: {message}"
    );

    assert_eq!(fs::read(&star)?, written, "{label}: plugin.star changed");
    Ok(())
}

/// Every extension except `except` keeps its origin, service request, and
/// record counts: a replacement moves one record and nothing else.
fn assert_untouched(
    before: &Product,
    after: &Product,
    except: &[&str],
) -> Result<(), Box<dyn Error>> {
    for old in &before.extensions {
        if except.contains(&old.name()) {
            continue;
        }
        let new = extension(after, old.name())?;
        let label = old.name();
        assert_eq!(new.origin(), old.origin(), "{label} origin");
        assert_eq!(new.inject(), old.inject(), "{label} services");
        assert_eq!(new.skills().len(), old.skills().len(), "{label} skills");
        assert_eq!(new.rules().len(), old.rules().len(), "{label} rules");
        assert_eq!(new.tools().len(), old.tools().len(), "{label} tools");
        assert_eq!(
            new.prompt_sections().len(),
            old.prompt_sections().len(),
            "{label} prompt sections"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Controls: a clash without `replace` is refused today and must stay refused.
// ---------------------------------------------------------------------------

#[test]
fn a_user_skill_named_like_a_battery_skill_fails_the_build() -> Result<(), Box<dyn Error>> {
    let error = failure(&[skill_plugin("mine", BATTERY_SKILL, &Claim::None)])?;
    assert_mentions(&error, &[BATTERY_SKILL, "already registered"]);
    Ok(())
}

#[test]
fn a_user_rule_named_like_a_battery_rule_fails_host_start() -> Result<(), Box<dyn Error>> {
    let error = host_start_failure(&[rule_plugin("mine", BATTERY_RULE, &Claim::None)])?;
    assert_mentions(
        &error,
        &[
            r#"rule "fabricated-unavailable-tool-call" is already registered by plugin "quality""#,
            r#"plugin "mine" cannot register it"#,
        ],
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The explicit opt-in reaches the exact bundled owner and moves one record.
// ---------------------------------------------------------------------------

#[test]
fn replace_takes_one_battery_skill_and_keeps_the_battery() -> Result<(), Box<dyn Error>> {
    let baseline = built(&[])?;
    let product = built(&[skill_plugin(
        "mine",
        BATTERY_SKILL,
        &Claim::Owner(OWNER_SKILLS),
    )])?;

    let mut expected = names(&baseline);
    expected.insert("mine");
    assert_eq!(names(&product), expected);
    assert_eq!(product.extensions.len(), baseline.extensions.len() + 1);

    let mine = extension(&product, "mine")?;
    assert_eq!(mine.origin(), Origin::User);
    assert_eq!(skill_set(mine), BTreeSet::from([BATTERY_SKILL]));
    let taken = mine.skills().first().ok_or("mine registers no skill")?;
    assert_eq!(&*taken.body, SKILL_BODY);

    let skills = extension(&product, OWNER_SKILLS)?;
    assert_eq!(skills.origin(), Origin::Bundled);
    assert_eq!(
        skill_set(skills),
        BTreeSet::from(["delegate-with-contracts", "initializer-and-sprints"])
    );
    for record in skills.skills() {
        let body = match record.name.as_str() {
            "delegate-with-contracts" => DELEGATE_WITH_CONTRACTS_SKILL,
            "initializer-and-sprints" => INITIALIZER_AND_SPRINTS_SKILL,
            other => {
                return Err(io::Error::other(format!("unexpected battery skill {other}")).into());
            }
        };
        assert_eq!(&*record.body, body, "{}", record.name);
    }

    assert_untouched(&baseline, &product, &[OWNER_SKILLS, "mine"])
}

#[test]
fn the_replaced_skill_is_the_published_skill_and_the_other_battery_skills_stay()
-> Result<(), Box<dyn Error>> {
    with_host(
        &[skill_plugin(
            "mine",
            BATTERY_SKILL,
            &Claim::Owner(OWNER_SKILLS),
        )],
        |host| {
            let replaced = host.doc("skill://find-anything")?;
            assert_eq!(&*replaced.text, SKILL_BODY);
            let kept = host.doc("skill://delegate-with-contracts")?;
            assert_eq!(&*kept.text, DELEGATE_WITH_CONTRACTS_SKILL);
            let other = host.doc("skill://initializer-and-sprints")?;
            assert_eq!(&*other.text, INITIALIZER_AND_SPRINTS_SKILL);
            Ok(())
        },
    )
}

#[test]
fn replace_takes_one_battery_rule_without_the_batterys_grants_status_or_prompt()
-> Result<(), Box<dyn Error>> {
    let baseline = built(&[])?;
    let (root, config) = prepared(&[rule_plugin(
        "mine",
        BATTERY_RULE,
        &Claim::Owner(OWNER_QUALITY),
    )])?;
    let product = product_of(&root, &config)?;

    let before = extension(&baseline, OWNER_QUALITY)?;
    assert!(
        rule_set(before).contains(BATTERY_RULE),
        "precondition: quality registers {BATTERY_RULE}"
    );
    let quality = extension(&product, OWNER_QUALITY)?;
    let mine = extension(&product, "mine")?;

    // One record moved; the battery keeps every other record and its authority.
    let mut remaining = rule_set(before);
    remaining.remove(BATTERY_RULE);
    assert_eq!(rule_set(quality), remaining);
    assert_eq!(quality.origin(), Origin::Bundled);
    assert_eq!(quality.inject(), before.inject());
    assert_eq!(quality.tools().len(), before.tools().len());
    assert_eq!(
        quality.prompt_sections().len(),
        before.prompt_sections().len()
    );
    assert_eq!(
        quality.status().map(|(kind, _)| kind),
        before.status().map(|(kind, _)| kind)
    );

    // The replacement is a plain user plugin: no service request, no tools,
    // no prompt section, no status kind, and its own user grant identity.
    assert_eq!(mine.origin(), Origin::User);
    assert_eq!(rule_set(mine), BTreeSet::from([BATTERY_RULE]));
    assert_eq!(mine.skills(), &[]);
    assert_eq!(mine.inject(), ServiceSet::EMPTY);
    assert!(mine.tools().is_empty());
    assert!(mine.prompt_sections().is_empty());
    assert!(mine.status().is_none());

    // Grant identity: the battery's key is unchanged by the prune, and the
    // replacement's key, when the loader mints one, is User with no services
    // and can never equal the battery's.
    let workspace = root.0.clone();
    let before_key = GrantKey::for_extension(before, &workspace);
    let after_key = GrantKey::for_extension(quality, &workspace);
    assert_eq!(before_key, after_key, "battery grant identity");
    if let Some(mine_key) = GrantKey::for_extension(mine, &workspace) {
        assert_eq!(mine_key.origin, Origin::User);
        assert_eq!(mine_key.services, ServiceSet::EMPTY);
        assert_ne!(Some(mine_key), after_key);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The flag binds to the exact bundled owner; every other owner refuses.
// ---------------------------------------------------------------------------

#[test]
fn replace_of_a_skill_the_named_battery_does_not_register_is_refused() -> Result<(), Box<dyn Error>>
{
    let error = failure(&[skill_plugin(
        "mine",
        "not-a-battery-skill",
        &Claim::Owner(OWNER_SKILLS),
    )])?;
    assert_mentions(
        &error,
        &[
            r#"skill "not-a-battery-skill" cannot be replaced by plugin "mine""#,
            r#"bundled extension "skills" does not register it"#,
            // The refusal is a load error: it carries the plugin.star site.
            "plugin.star",
        ],
    );
    Ok(())
}

#[test]
fn replace_naming_a_battery_this_product_does_not_ship_is_refused() -> Result<(), Box<dyn Error>> {
    let error = failure(&[skill_plugin(
        "mine",
        BATTERY_SKILL,
        &Claim::Owner("absent-battery"),
    )])?;
    assert_mentions(
        &error,
        &[
            r#"skill "find-anything" cannot be replaced by plugin "mine""#,
            r#"this product ships no bundled extension "absent-battery""#,
        ],
    );
    Ok(())
}

#[test]
fn replace_naming_the_wrong_battery_is_refused_even_though_another_battery_owns_the_record()
-> Result<(), Box<dyn Error>> {
    let error = failure(&[rule_plugin(
        "mine",
        BATTERY_RULE,
        &Claim::Owner("ttsr-rules"),
    )])?;
    let record = format!(r#"rule "{BATTERY_RULE}" cannot be replaced by plugin "mine""#);
    assert_mentions(
        &error,
        &[
            record.as_str(),
            r#"bundled extension "ttsr-rules" does not register it"#,
            r#"it is registered by plugin "quality""#,
        ],
    );
    Ok(())
}

#[test]
fn replace_naming_a_builtin_extension_is_refused() -> Result<(), Box<dyn Error>> {
    let error = failure(&[skill_plugin("mine", BATTERY_SKILL, &Claim::Owner("tools"))])?;
    assert_mentions(
        &error,
        &[
            r#"skill "find-anything" cannot be replaced by plugin "mine""#,
            r#"extension "tools" is a built-in extension"#,
        ],
    );
    Ok(())
}

#[test]
fn replace_naming_a_user_plugin_is_refused() -> Result<(), Box<dyn Error>> {
    let error = failure(&[
        skill_plugin("mine", "shared-skill", &Claim::Owner("other")),
        skill_plugin("other", "shared-skill", &Claim::None),
    ])?;
    assert_mentions(
        &error,
        &[
            r#"skill "shared-skill" cannot be replaced by plugin "mine""#,
            r#"extension "other" is a user plugin"#,
        ],
    );
    Ok(())
}

#[test]
fn two_user_plugins_cannot_replace_the_same_battery_record() -> Result<(), Box<dyn Error>> {
    let error = failure(&[
        skill_plugin("alpha", BATTERY_SKILL, &Claim::Owner(OWNER_SKILLS)),
        skill_plugin("beta", BATTERY_SKILL, &Claim::Owner(OWNER_SKILLS)),
    ])?;
    assert_mentions(
        &error,
        &[
            r#"skill "find-anything" cannot be replaced by plugin "beta""#,
            r#"already replaced by plugin "alpha""#,
        ],
    );
    Ok(())
}

#[test]
fn a_plugin_named_like_a_battery_stays_rejected_even_with_replace() -> Result<(), Box<dyn Error>> {
    let error = failure(&[skill_plugin(
        OWNER_SKILLS,
        BATTERY_SKILL,
        &Claim::Owner(OWNER_SKILLS),
    )])?;
    assert_mentions(
        &error,
        &[r#"extension "skills" is already registered by plugin "skills""#],
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Strict input: the two keywords stand or fall together.
// ---------------------------------------------------------------------------

#[test]
fn replace_and_battery_are_inseparable_strict_input() -> Result<(), Box<dyn Error>> {
    let defaulted = failure(&[skill_plugin("mine", BATTERY_SKILL, &Claim::Defaulted)])?;
    assert_mentions(&defaulted, &[BATTERY_SKILL, "already registered"]);

    let owner_only = failure(&[skill_plugin(
        "mine",
        BATTERY_SKILL,
        &Claim::OwnerWithoutReplace(OWNER_SKILLS),
    )])?;
    assert_mentions(&owner_only, &["battery", "replace"]);

    let replace_only = failure(&[skill_plugin(
        "mine",
        BATTERY_SKILL,
        &Claim::ReplaceWithoutOwner,
    )])?;
    assert_mentions(&replace_only, &["replace", "battery"]);
    Ok(())
}

// ---------------------------------------------------------------------------
// Strict types: each keyword refuses a value of another type at the call.
// ---------------------------------------------------------------------------

/// Values a `replace` flag must refuse. An explicit `None` is one of them:
/// the SDK declares `replace` as an optional `bool`, and Starlark 0.14.2
/// treats an omitted optional argument as absent but an explicit `None` as a
/// value of the wrong type, so it never reaches the pair refusal.
const NOT_A_BOOL: [&str; 3] = ["1", "\"True\"", "None"];

/// Values a `battery` owner must refuse, `None` included for the same reason.
const NOT_A_STRING: [&str; 2] = ["5", "None"];

#[test]
fn a_replace_flag_that_is_not_a_bool_is_refused_at_the_call() -> Result<(), Box<dyn Error>> {
    for literal in NOT_A_BOOL {
        let skill = Claim::WrongFlag {
            literal,
            owner: OWNER_SKILLS,
        };
        assert_wrong_type(&skill_plugin("mine", BATTERY_SKILL, &skill), &skill)?;
        let rule = Claim::WrongFlag {
            literal,
            owner: OWNER_QUALITY,
        };
        assert_wrong_type(&rule_plugin("mine", BATTERY_RULE, &rule), &rule)?;
    }
    Ok(())
}

#[test]
fn a_battery_owner_that_is_not_a_string_is_refused_at_the_call() -> Result<(), Box<dyn Error>> {
    for literal in NOT_A_STRING {
        let claim = Claim::WrongOwner { literal };
        assert_wrong_type(&skill_plugin("mine", BATTERY_SKILL, &claim), &claim)?;
        assert_wrong_type(&rule_plugin("mine", BATTERY_RULE, &claim), &claim)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reload atomicity: one settle, one publish. A refused reload keeps the old
// generation and the actual old `skill://` bodies; a successful one serves the
// records the plugin files now declare, and every untouched record keeps its
// body.
// ---------------------------------------------------------------------------

#[test]
fn replace_false_on_a_fresh_name_loads_plainly() -> Result<(), Box<dyn Error>> {
    let baseline = built(&[])?;
    let product = built(&[skill_plugin("mine", FRESH_SKILL, &Claim::Defaulted)])?;

    let mut expected = names(&baseline);
    expected.insert("mine");
    assert_eq!(names(&product), expected);
    let mine = extension(&product, "mine")?;
    assert_eq!(mine.origin(), Origin::User);
    assert_eq!(skill_set(mine), BTreeSet::from([FRESH_SKILL]));
    let fresh = mine.skills().first().ok_or("mine registers no skill")?;
    assert_eq!(&*fresh.body, SKILL_BODY);
    assert_untouched(&baseline, &product, &["mine"])?;

    // Served: the fresh record is published and no bundled skill moved.
    with_session(
        &[skill_plugin("mine", FRESH_SKILL, &Claim::Defaulted)],
        async |host, _agent| -> Result<(), Box<dyn Error>> {
            assert_served(host, "skill://fresh-skill", SKILL_BODY)?;
            assert_battery_skills(host, FIND_ANYTHING_SKILL)
        },
    )
}

#[test]
fn a_failed_reload_keeps_the_old_generation_and_skill_bodies() -> Result<(), Box<dyn Error>> {
    with_session(
        &[
            skill_plugin("mine", BATTERY_SKILL, &Claim::Owner(OWNER_SKILLS)),
            other_plugin(),
        ],
        async |host, agent| -> Result<(), Box<dyn Error>> {
            let before = host.generation();
            assert_battery_skills(host, SKILL_BODY)?;
            assert_served(host, "skill://other-skill", OTHER_BODY)?;

            // `mine` now names a bundled owner this product does not ship, with
            // a new body beside it, and `other` carries an edit that is valid
            // on its own: the refused reload must publish neither body.
            edit_plugin(
                host,
                &with_files(
                    skill_plugin("mine", BATTERY_SKILL, &Claim::Owner("absent-battery")),
                    &[("SKILL.md", EDITED_BODY)],
                ),
            )?;
            edit_plugin(
                host,
                &with_files(
                    skill_plugin("other", OTHER_SKILL, &Claim::None),
                    &[("SKILL.md", EDITED_BODY)],
                ),
            )?;
            let refusal = reload_failure(agent).await?;
            assert_mentions(&refusal, &[BATTERY_SKILL, "absent-battery"]);

            assert_eq!(
                host.generation(),
                before,
                "the refused reload kept the old generation"
            );
            assert_battery_skills(host, SKILL_BODY)?;
            assert_served(host, "skill://other-skill", OTHER_BODY)
        },
    )
}

#[test]
fn a_reload_without_the_claims_restores_both_battery_records() -> Result<(), Box<dyn Error>> {
    with_session(
        &[two_skill_plugin(
            "delegate-with-contracts",
            &Claim::Owner(OWNER_SKILLS),
            "find-anything",
            &Claim::Owner(OWNER_SKILLS),
        )],
        async |host, agent| -> Result<(), Box<dyn Error>> {
            let before = host.generation();
            assert_served(host, "skill://delegate-with-contracts", FIRST_BODY)?;
            assert_served(host, "skill://find-anything", SECOND_BODY)?;
            assert_served(
                host,
                "skill://initializer-and-sprints",
                INITIALIZER_AND_SPRINTS_SKILL,
            )?;

            // Both claims go, and so do the records that took the bundled
            // names: keeping a name without its claim is the clash that the
            // next case refuses.
            edit_plugin(
                host,
                &two_skill_plugin("moved-first", &Claim::None, "moved-second", &Claim::None),
            )?;
            reloaded(agent).await?;

            assert_ne!(host.generation(), before, "the reload published");
            assert_battery_skills(host, FIND_ANYTHING_SKILL)?;
            assert_served(host, "skill://moved-first", FIRST_BODY)?;
            assert_served(host, "skill://moved-second", SECOND_BODY)
        },
    )
}

#[test]
fn dropping_the_claim_but_keeping_the_record_refuses_and_keeps_the_old_live()
-> Result<(), Box<dyn Error>> {
    with_session(
        &[skill_plugin(
            "mine",
            BATTERY_SKILL,
            &Claim::Owner(OWNER_SKILLS),
        )],
        async |host, agent| -> Result<(), Box<dyn Error>> {
            let before = host.generation();
            assert_battery_skills(host, SKILL_BODY)?;

            // The claim is gone but the record still takes the bundled name,
            // with a new body beside it: the clash is back, and the old
            // replacement body stays live.
            edit_plugin(
                host,
                &with_files(
                    skill_plugin("mine", BATTERY_SKILL, &Claim::None),
                    &[("SKILL.md", EDITED_BODY)],
                ),
            )?;
            let refusal = reload_failure(agent).await?;
            assert_mentions(&refusal, &[BATTERY_SKILL, "already registered"]);

            assert_eq!(
                host.generation(),
                before,
                "the refused reload kept the old generation"
            );
            assert_battery_skills(host, SKILL_BODY)
        },
    )
}

#[test]
fn a_successful_reload_loses_no_untouched_record() -> Result<(), Box<dyn Error>> {
    with_session(
        &[
            skill_plugin("mine", BATTERY_SKILL, &Claim::Owner(OWNER_SKILLS)),
            other_plugin(),
        ],
        async |host, agent| -> Result<(), Box<dyn Error>> {
            let before = host.generation();
            assert_battery_skills(host, SKILL_BODY)?;
            assert_served(host, "skill://other-skill", OTHER_BODY)?;

            // Only the replacing plugin's body changes on disk; its claim stays.
            edit_plugin(
                host,
                &with_files(
                    skill_plugin("mine", BATTERY_SKILL, &Claim::Owner(OWNER_SKILLS)),
                    &[("SKILL.md", EDITED_BODY)],
                ),
            )?;
            reloaded(agent).await?;

            assert_ne!(host.generation(), before, "the reload published");
            // The reload re-read the edited file, kept the replacement, and
            // left every unrelated user and bundled body as it was.
            assert_battery_skills(host, EDITED_BODY)?;
            assert_served(host, "skill://other-skill", OTHER_BODY)
        },
    )
}
