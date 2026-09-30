//! Scan, evaluate, freeze, and validate one plugin generation (spec §P03).
//!
//! The pipeline reads `<data>/plugins/` plus bundled sources, evaluates each
//! `plugin.star` under the load budget, freezes the module, and validates the
//! exported `plugin` descriptor. `@dal/v1` is the only virtual module; any
//! other `@…` label or an entry that never loads `@dal/v1` fails as
//! [`LoadError::UnsupportedEntry`]. The first failure in plugin order stops
//! the whole load — a generation is published whole or not at all.

use dal_core::Origin;
use starlark::{
    codemap::Span,
    environment::{FrozenModule, Globals, Module},
    eval::{Evaluator, FileLoader},
    syntax::AstModule,
    syntax::ast::{AssignTargetP, AstAssignTarget, AstStmt, StmtP},
};
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

use crate::engine::{Limits, MAX_CODE, MAX_NESTING, dialect, globals};
use crate::error::LoadError;
use crate::sdk::{DAL_V1, dal_v1_module};
use crate::system::PluginGeneration;
use crate::validate::{self, LoadedPlugin};

/// One bundled plugin source tree joined into the load pipeline.
#[derive(Clone, Debug)]
pub struct BundledPlugin {
    /// The plugin directory name the sources declare.
    pub name: String,
    /// Files keyed by plugin-relative paths, as carried by the product.
    pub files: BTreeMap<PathBuf, &'static [u8]>,
}

/// Roots scanned by [`load`].
#[derive(Clone, Debug)]
pub struct LoadRoots {
    /// The dal data directory holding `plugins/`.
    pub data_root: PathBuf,
    /// Bundled source trees evaluated before user plugins.
    pub bundled: Vec<BundledPlugin>,
}

/// Selection of plugins to load; empty means every discovered plugin.
#[derive(Clone, Debug, Default)]
pub struct PluginsConfig {
    /// Plugin names to load, in any order. Empty loads all.
    pub enabled: Vec<String>,
    /// Evaluator budgets from `[limits.plugins]`.
    pub limits: dal_core::PluginLimits,
    /// Resolved per-plugin configuration objects keyed by plugin name,
    /// serialized as JSON. An absent entry supplies no config.
    pub configs: BTreeMap<String, String>,
}

/// One plugin's source snapshot: the files it may load and reference.
struct FileSet {
    /// Display root for diagnostics (`<bundled>/<name>` or the real path).
    dir: PathBuf,
    /// Plugin-relative path to bytes, capped at [`MAX_CODE`] per file.
    files: BTreeMap<PathBuf, Vec<u8>>,
}

/// Serves frozen modules by the `load(...)` literal the requester wrote.
struct MapLoader<'a> {
    modules: &'a HashMap<String, FrozenModule>,
}

impl FileLoader for MapLoader<'_> {
    fn load(&self, path: &str) -> starlark::Result<FrozenModule> {
        self.modules.get(path).cloned().ok_or_else(|| {
            starlark::Error::new_other(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("unknown load module `{path}`"),
            ))
        })
    }
}

/// Runs the full pipeline and returns the validated generation.
///
/// # Errors
///
/// Returns [`LoadError`] for scan, evaluation, freeze, or validation failures.
pub fn load(roots: &LoadRoots, cfg: &PluginsConfig) -> Result<PluginGeneration, LoadError> {
    let globals = globals();
    let mut ordered: Vec<(String, Origin, FileSet)> = Vec::new();
    for plugin in &roots.bundled {
        let mut files = BTreeMap::new();
        for (path, bytes) in &plugin.files {
            check_size(&plugin.name, path, bytes.len())?;
            files.insert(path.clone(), bytes.to_vec());
        }
        ordered.push((
            plugin.name.clone(),
            Origin::Bundled,
            FileSet {
                dir: PathBuf::from(format!("<bundled>/{}", plugin.name)),
                files,
            },
        ));
    }

    let plugins_dir = roots.data_root.join("plugins");
    match std::fs::read_dir(&plugins_dir) {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(LoadError::Eval {
                path: plugins_dir.clone(),
                line: 1,
                col: 1,
                message: format!("unreadable: {source}").into(),
            });
        }
        Ok(_) => {
            for name in sorted_plugin_dirs(&plugins_dir)? {
                if !valid_dir_name(&name) {
                    return Err(LoadError::InvalidDirectoryName {
                        name: name.into_boxed_str(),
                    });
                }
                if !cfg.enabled.is_empty() && !cfg.enabled.iter().any(|keep| keep == &name) {
                    continue;
                }
                let dir = plugins_dir.join(&name);
                let files = read_plugin_dir(&dir)?;
                if !files.contains_key(Path::new("plugin.star")) {
                    return Err(LoadError::MissingEntryFile { dir: dir.clone() });
                }
                ordered.push((name, Origin::User, FileSet { dir, files }));
            }
        }
    }

    ordered.sort_by(|left, right| {
        (origin_rank(left.1), &left.0).cmp(&(origin_rank(right.1), &right.0))
    });

    // Later origins shadow earlier ones by name: a user plugin replaces a
    // same-named bundled plugin rather than silently merging (§P02).
    let mut plugins: BTreeMap<String, LoadedPlugin> = BTreeMap::new();
    for (name, origin, files) in &ordered {
        let loaded = load_one(name, *origin, files, &globals, cfg)?;
        if plugins
            .insert(loaded.name.as_str().to_owned(), loaded)
            .is_some()
        {
            tracing::info!(
                plugin = %name,
                "plugin shadows an earlier plugin with the same name"
            );
        }
    }
    Ok(PluginGeneration::new(
        plugins.into_values().map(std::sync::Arc::new).collect(),
    ))
}

/// Maps configured budgets onto one load evaluation.
fn load_limits(limits: &dal_core::PluginLimits) -> Limits {
    Limits {
        ticks: limits.load_ticks,
        heap_bytes: limits.load_heap_bytes,
        stack_depth: limits.stack_depth,
    }
}

/// Maps configured budgets onto one handler invocation.
fn handler_limits(limits: &dal_core::PluginLimits) -> Limits {
    Limits {
        ticks: limits.handler_ticks,
        heap_bytes: limits.handler_heap_bytes,
        stack_depth: limits.stack_depth,
    }
}

fn origin_rank(origin: Origin) -> u8 {
    match origin {
        Origin::Bundled => 0,
        Origin::User => 1,
        _ => 2,
    }
}

fn sorted_plugin_dirs(plugins_dir: &Path) -> Result<Vec<String>, LoadError> {
    let read_dir = std::fs::read_dir(plugins_dir).map_err(|source| LoadError::Eval {
        path: plugins_dir.to_path_buf(),
        line: 1,
        col: 1,
        message: format!("unreadable: {source}").into(),
    })?;
    let mut names = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|source| LoadError::Eval {
            path: plugins_dir.to_path_buf(),
            line: 1,
            col: 1,
            message: format!("unreadable: {source}").into(),
        })?;
        if entry.path().is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Rejects a file above the per-file source budget.
fn check_size(owner: &str, path: &Path, bytes: usize) -> Result<(), LoadError> {
    if bytes <= MAX_CODE {
        return Ok(());
    }
    Err(LoadError::Eval {
        path: path.to_path_buf(),
        line: 1,
        col: 1,
        message: format!("file exceeds {MAX_CODE} bytes in plugin `{owner}`").into(),
    })
}

fn read_plugin_dir(dir: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, LoadError> {
    let mut files = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    let owner = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    while let Some(current) = stack.pop() {
        let read_dir = std::fs::read_dir(&current).map_err(|source| LoadError::Eval {
            path: current.clone(),
            line: 1,
            col: 1,
            message: format!("unreadable: {source}").into(),
        })?;
        for entry in read_dir {
            let entry = entry.map_err(|source| LoadError::Eval {
                path: current.clone(),
                line: 1,
                col: 1,
                message: format!("unreadable: {source}").into(),
            })?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|source| LoadError::Eval {
                path: path.clone(),
                line: 1,
                col: 1,
                message: format!("unreadable: {source}").into(),
            })?;
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                let relative = path.strip_prefix(dir).map_err(|_| LoadError::Eval {
                    path: path.clone(),
                    line: 1,
                    col: 1,
                    message: "unreadable: path escapes plugin directory".into(),
                })?;
                let bytes = std::fs::read(&path).map_err(|source| LoadError::Unreadable {
                    path: path.clone(),
                    source,
                })?;
                check_size(&owner, relative, bytes.len())?;
                files.insert(relative.to_path_buf(), bytes);
            }
        }
    }
    Ok(files)
}

/// Evaluates, freezes and validates one plugin.
fn load_one(
    name: &str,
    origin: Origin,
    files: &FileSet,
    globals: &Globals,
    cfg: &PluginsConfig,
) -> Result<LoadedPlugin, LoadError> {
    let entry_path = files.dir.join("plugin.star");
    let entry_bytes =
        files
            .files
            .get(Path::new("plugin.star"))
            .ok_or_else(|| LoadError::MissingEntryFile {
                dir: files.dir.clone(),
            })?;
    let entry_text = utf8(&entry_path, entry_bytes)?;
    let mut loader = ModuleLoader {
        files,
        globals,
        limits: load_limits(&cfg.limits),
        stack: Vec::new(),
    };
    let frozen = loader.module(&entry_path, "plugin.star", entry_text, 0)?;

    // The v1 authoring contract (§P01, §P07): the root exports exactly one
    // `plugin` value, reachable only through `load("@dal/v1", ...)`.
    let plugin_value = frozen
        .get_option("plugin")
        .map_err(|error| LoadError::Eval {
            path: entry_path.clone(),
            line: 1,
            col: 1,
            message: error.to_string().into(),
        })?
        .ok_or_else(|| LoadError::UnsupportedEntry {
            path: entry_path.clone(),
        })?;

    let dir = files.dir.to_string_lossy();
    validate::validate(
        &dir,
        name,
        origin,
        &plugin_value,
        &files.files,
        cfg.configs.get(name).map(String::as_str),
        handler_limits(&cfg.limits),
    )
}

/// Rejects an entry module that binds `plugin` more than once (§P07).
///
/// Starlark lets a module rebind a global, so the last binding would win
/// silently; the exported identity must come from exactly one binding.
fn reject_rebound_plugin(ast: &AstModule, path: &Path) -> Result<(), LoadError> {
    let mut spans = Vec::new();
    plugin_bindings(ast.statement(), &mut spans);
    let Some(second) = spans.get(1) else {
        return Ok(());
    };
    let begin = ast.file_span(*second).resolve_span().begin;
    Err(LoadError::PluginRebound {
        path: path.to_path_buf(),
        line: u32::try_from(begin.line + 1).unwrap_or(u32::MAX),
        col: u32::try_from(begin.column + 1).unwrap_or(u32::MAX),
    })
}

/// Collects, in source order, the module-level statements that bind the
/// name `plugin`; function bodies are their own scope and are skipped.
fn plugin_bindings(stmt: &AstStmt, out: &mut Vec<Span>) {
    match &stmt.node {
        StmtP::Assign(assign) => target_bindings(&assign.lhs, out),
        StmtP::AssignModify(target, _, _) => target_bindings(target, out),
        StmtP::Statements(stmts) => stmts.iter().for_each(|stmt| plugin_bindings(stmt, out)),
        StmtP::If(_, body) => plugin_bindings(body, out),
        StmtP::IfElse(_, bodies) => {
            plugin_bindings(&bodies.0, out);
            plugin_bindings(&bodies.1, out);
        }
        StmtP::For(each) => {
            target_bindings(&each.var, out);
            plugin_bindings(&each.body, out);
        }
        StmtP::Def(def) if def.name.node.ident == "plugin" => out.push(def.name.span),
        StmtP::Load(load) => out.extend(
            load.args
                .iter()
                .filter(|arg| arg.local.node.ident == "plugin")
                .map(|arg| arg.local.span),
        ),
        _ => {}
    }
}

/// Collects the `plugin` identifiers an assignment target binds.
fn target_bindings(target: &AstAssignTarget, out: &mut Vec<Span>) {
    match &target.node {
        AssignTargetP::Identifier(ident) if ident.node.ident == "plugin" => out.push(target.span),
        AssignTargetP::Tuple(items) => items.iter().for_each(|item| target_bindings(item, out)),
        _ => {}
    }
}

/// Reads module source as UTF-8.
fn utf8<'b>(path: &Path, bytes: &'b [u8]) -> Result<&'b str, LoadError> {
    std::str::from_utf8(bytes).map_err(|_| LoadError::InvalidUtf8 {
        path: path.to_string_lossy().into_owned().into_boxed_str(),
    })
}

/// One plugin's module walk. The files, globals, and budget stay fixed; the
/// stack holds the current `load` chain for the cycle and depth checks.
struct ModuleLoader<'a> {
    /// The plugin's source snapshot.
    files: &'a FileSet,
    /// The evaluation globals.
    globals: &'a Globals,
    /// The per-module load budget.
    limits: Limits,
    /// The module names on the current `load` chain.
    stack: Vec<String>,
}

impl ModuleLoader<'_> {
    /// Parses, resolves dependencies, evaluates and freezes one module.
    fn module(
        &mut self,
        display_path: &Path,
        module_name: &str,
        source: &str,
        depth: usize,
    ) -> Result<FrozenModule, LoadError> {
        if u8::try_from(depth).unwrap_or(u8::MAX) > MAX_NESTING {
            return Err(LoadError::LoadDepth {
                module: module_name.into(),
            });
        }
        if self.stack.iter().any(|seen| seen == module_name) {
            return Err(LoadError::LoadCycle {
                module: module_name.into(),
            });
        }
        self.stack.push(module_name.to_owned());
        let result = self.framed(display_path, source, depth);
        self.stack.pop();
        result
    }

    /// The body of [`Self::module`]; the caller owns stack hygiene.
    fn framed(
        &mut self,
        display_path: &Path,
        source: &str,
        depth: usize,
    ) -> Result<FrozenModule, LoadError> {
        let ast = AstModule::parse(
            &display_path.to_string_lossy(),
            source.to_owned(),
            &dialect(),
        )
        .map_err(|error| LoadError::Eval {
            path: display_path.to_path_buf(),
            line: 1,
            col: 1,
            message: error.to_string().into(),
        })?;
        let wants: Vec<String> = ast
            .loads()
            .iter()
            .map(|item| item.module_id.to_owned())
            .collect();

        // §P07: the entry module loads @dal/v1 exactly once; any other
        // virtual label is an unsupported-entry failure at every depth.
        let sdk_loads = wants.iter().filter(|want| want.as_str() == DAL_V1).count();
        if depth == 0 && sdk_loads != 1 {
            return Err(LoadError::UnsupportedEntry {
                path: display_path.to_path_buf(),
            });
        }
        if depth == 0 {
            reject_rebound_plugin(&ast, display_path)?;
        }
        if let Some(other) = wants
            .iter()
            .find(|want| want.starts_with('@') && want.as_str() != DAL_V1)
        {
            return Err(LoadError::SdkModule {
                module: other.as_str().into(),
                message: format!("only {DAL_V1} is a supported virtual module").into(),
            });
        }

        let mut frozen_deps: HashMap<String, FrozenModule> = HashMap::new();
        for want in wants {
            let frozen = self.dependency(&want, depth)?;
            frozen_deps.insert(want, frozen);
        }
        let loader = MapLoader {
            modules: &frozen_deps,
        };
        try_evaluate(&ast, display_path, self.globals, self.limits, &loader)
    }

    /// Freezes one `load` target: the SDK module or a plugin-local file.
    fn dependency(&mut self, want: &str, depth: usize) -> Result<FrozenModule, LoadError> {
        if want == DAL_V1 {
            return Ok(dal_v1_module()?.clone());
        }
        let (resolved, bytes) = resolve_load(want, self.files)?;
        let path = self.files.dir.join(&resolved);
        let text = utf8(&path, bytes)?;
        self.module(&path, want, text, depth + 1)
    }
}

fn try_evaluate(
    ast: &AstModule,
    display_path: &Path,
    globals: &Globals,
    limits: Limits,
    loader: &dyn FileLoader,
) -> Result<FrozenModule, LoadError> {
    Module::with_temp_heap(|module| {
        {
            let mut evaluator = Evaluator::new(&module);
            evaluator.set_loader(loader);
            apply_limits(&mut evaluator, display_path, limits)?;
            evaluator
                .eval_module(ast.clone(), globals)
                .map_err(|error| load_eval_error(display_path, &error.to_string()))?;
        }
        module.freeze().map_err(|error| LoadError::Eval {
            path: display_path.to_path_buf(),
            line: 1,
            col: 1,
            message: format!("{error:?}").into(),
        })
    })
}

/// The plugin-name grammar for directory names (§P02).
fn valid_dir_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

fn load_eval_error(display_path: &Path, message: &str) -> LoadError {
    if let Some(name) = unknown_module_name(message) {
        return LoadError::MissingModule {
            module: name.into_boxed_str(),
        };
    }
    LoadError::Eval {
        path: display_path.to_path_buf(),
        line: 1,
        col: 1,
        message: message.to_owned().into(),
    }
}

fn unknown_module_name(message: &str) -> Option<String> {
    message
        .split("unknown load module")
        .nth(1)
        .map(|rest| {
            rest.trim()
                .trim_matches(|char| char == '`' || char == '\'' || char == '"' || char == ')')
                .to_owned()
        })
        .filter(|name| !name.is_empty())
}

fn apply_limits(
    evaluator: &mut Evaluator<'_, '_, '_>,
    display_path: &Path,
    limits: Limits,
) -> Result<(), LoadError> {
    let heap = usize::try_from(limits.heap_bytes).unwrap_or(usize::MAX);
    let depth = usize::try_from(limits.stack_depth).unwrap_or(usize::MAX);
    evaluator
        .set_max_tick_count(limits.ticks)
        .map_err(|error| LoadError::Eval {
            path: display_path.to_path_buf(),
            line: 1,
            col: 1,
            message: error.to_string().into(),
        })?;
    evaluator
        .set_max_heap_size(heap)
        .map_err(|error| LoadError::Eval {
            path: display_path.to_path_buf(),
            line: 1,
            col: 1,
            message: error.to_string().into(),
        })?;
    evaluator
        .set_max_callstack_size(depth)
        .map_err(|error| LoadError::Eval {
            path: display_path.to_path_buf(),
            line: 1,
            col: 1,
            message: error.to_string().into(),
        })?;
    Ok(())
}

/// Resolves a relative `load(...)` inside the plugin's own source snapshot.
///
/// In-memory resolution is the confinement: the `FileSet` only ever contains
/// files beneath the plugin root, so a resolved path cannot escape the
/// directory the scan admitted, for bundled and user plugins alike.
fn resolve_load<'f>(want: &str, files: &'f FileSet) -> Result<(PathBuf, &'f [u8]), LoadError> {
    if want.contains('\\') || Path::new(want).is_absolute() {
        return Err(LoadError::LoadEscape {
            module: want.to_owned().into_boxed_str(),
        });
    }
    let candidate = PathBuf::from(want);
    if candidate
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(LoadError::LoadEscape {
            module: want.to_owned().into_boxed_str(),
        });
    }
    let Some(bytes) = files.files.get(&candidate) else {
        return Err(LoadError::MissingModule {
            module: want.to_owned().into_boxed_str(),
        });
    };
    Ok((candidate, bytes.as_slice()))
}
