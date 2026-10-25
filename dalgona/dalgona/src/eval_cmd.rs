// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! `dalgona eval`: runs an eval pack and writes its report.
//!
//! The command belongs to the dalgona binary. It resolves the pack, starts
//! one host per arm over the same product `dalgona` runs, and hands the run to
//! the pack runner in `dalgona-batteries`. It reads the process environment
//! once, here at the edge.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Display;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use dalgona_batteries::eval::pack::{self, Pack};
use dalgona_batteries::eval::report::{Report, RunLine, Status};
use dalgona_batteries::eval::resolve;
use dalgona_batteries::eval::run::{self, Arm, ExecError, Executor, Options};
use dalgona_batteries::eval::{CancellationToken, Stamp, USAGE};

use crate::eval_host::{ArmHost, ArmInputs, load_config};

type Vars = BTreeMap<OsString, OsString>;

const HELP: &str = "Run an eval pack: a set of cases, each one prompt, seed files, and a check.\n\n    dalgona eval <pack> [--case <id>] [--repeats <n>] [--model <id>] [--against <dal.toml>]\n\n  --case <id>              run one case\n  --repeats <n>            run each case n times (1 to 10)\n  --model <id>             the model to run; defaults to `model` in dal.toml\n  --against <dal.toml>     also run every case with this file layered over your config\n\nSee dalgona://eval.\n";

/// The parsed command line.
#[derive(Debug)]
struct Args {
    pack: String,
    case: Option<String>,
    repeats: Option<u32>,
    model: Option<String>,
    against: Option<PathBuf>,
}

#[derive(Debug)]
enum Parsed {
    Help,
    Run(Args),
}

#[derive(Debug, thiserror::Error)]
enum UsageError {
    #[error("dalgona eval: the pack name is missing")]
    MissingPack,
    #[error("dalgona eval: {0} needs a value")]
    MissingValue(&'static str),
    #[error("dalgona eval: unknown option {0}")]
    Unknown(String),
    #[error("dalgona eval: unexpected argument {0}; one pack at a time")]
    ExtraPack(String),
    #[error("dalgona eval: {0} is not valid UTF-8")]
    NotUtf8(String),
    #[error("dalgona eval: --repeats must be an integer from 1 to 10")]
    Repeats,
}

fn text(value: OsString) -> Result<String, UsageError> {
    value
        .into_string()
        .map_err(|raw| UsageError::NotUtf8(raw.to_string_lossy().into_owned()))
}

/// Splits `--flag=value` into its parts.
fn split(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
        _ => (arg, None),
    }
}

fn parse(args: &[OsString]) -> Result<Parsed, UsageError> {
    let mut pack = None;
    let mut case = None;
    let mut repeats = None;
    let mut model = None;
    let mut against = None;
    let mut rest = args.iter().cloned();
    while let Some(arg) = rest.next() {
        let arg = text(arg)?;
        let (flag, inline) = split(&arg);
        let mut value = |name: &'static str| match inline {
            Some(value) => Ok(OsString::from(value)),
            None => rest.next().ok_or(UsageError::MissingValue(name)),
        };
        match flag {
            "--help" | "-h" => return Ok(Parsed::Help),
            "--case" => case = Some(text(value("--case")?)?),
            "--model" => model = Some(text(value("--model")?)?),
            "--against" => against = Some(PathBuf::from(value("--against")?)),
            "--repeats" => {
                let raw = text(value("--repeats")?)?;
                repeats = Some(raw.parse::<u32>().map_err(|_| UsageError::Repeats)?);
            }
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(UsageError::Unknown(other.to_owned()));
            }
            _ if pack.is_none() => pack = Some(arg),
            _ => return Err(UsageError::ExtraPack(arg)),
        }
    }
    Ok(Parsed::Run(Args {
        pack: pack.ok_or(UsageError::MissingPack)?,
        case,
        repeats,
        model,
        against,
    }))
}

/// The directories `dalgona` itself reads and writes.
#[derive(Clone)]
struct Roots {
    config_file: PathBuf,
    data: PathBuf,
}

/// Projects the shared process-edge roots for the `dalgona` binary, so eval
/// accepts exactly the environments normal startup accepts and reads the same
/// config file and data root.
fn roots(vars: &Vars) -> Result<Roots, dalgon::EdgeError> {
    let shared = dalgon::resolve_roots(vars, crate::NAME)?;
    Ok(Roots {
        config_file: dalgon::config_file_path(&shared),
        data: shared.data,
    })
}

/// Names what to fix when the roots do not resolve; the home hint lists the
/// variables the shared resolver reads on this platform.
fn root_failure(error: &dalgon::EdgeError) -> String {
    match error {
        dalgon::EdgeError::HomeMissing if cfg!(windows) => "cannot find your home directory: \
             set HOME, USERPROFILE, or HOMEDRIVE and HOMEPATH to an absolute path"
            .to_owned(),
        dalgon::EdgeError::HomeMissing => {
            "cannot find your home directory: set HOME to an absolute path".to_owned()
        }
        other => format!("cannot resolve the dalgona directories: {other}"),
    }
}

fn fail(who: &str, reason: impl Display) -> ExitCode {
    let _ = writeln!(std::io::stderr().lock(), "{who}: {reason}");
    ExitCode::from(1)
}

/// Reads a config file; a missing file is no file.
fn read_optional(path: &Path) -> Result<Option<String>, ExecError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ExecError::Arm(
            format!("cannot read {}: {error}", path.display()).into(),
        )),
    }
}

/// Runs `dalgona eval`. `args` are the arguments after `eval`.
pub(crate) fn run(args: &[OsString]) -> ExitCode {
    let args = match parse(args) {
        Ok(Parsed::Help) => {
            let _ = write!(std::io::stdout().lock(), "{HELP}");
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Run(args)) => args,
        Err(error) => {
            let _ = writeln!(std::io::stderr().lock(), "{error}\nUsage: {USAGE}");
            return ExitCode::from(1);
        }
    };
    let vars: Vars = std::env::vars_os().collect();
    let roots = match roots(&vars) {
        Ok(roots) => roots,
        Err(error) => return fail(&args.pack, root_failure(&error)),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return fail(&args.pack, format!("cannot start the runtime: {error}")),
    };
    let code = runtime.block_on(execute(args, vars, roots));
    runtime.shutdown_timeout(std::time::Duration::from_millis(500));
    code
}

/// Runs synchronous file work off the async runtime.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => value,
        Err(error) => std::panic::resume_unwind(error.into_panic()),
    }
}

/// A pack that loaded, with the name it runs under.
struct Loaded {
    name: String,
    pack: Arc<Pack>,
}

fn load_pack(requested: &str, roots: &Roots) -> Result<Loaded, ExitCode> {
    let evals = roots.data.join("evals");
    let resolved = resolve::resolve(requested, &evals).map_err(|error| fail(requested, error))?;
    let file = format!("{}{}", resolved.name, resolve::PACK_SUFFIX);
    let pack = pack::load(&file, &resolved.source).map_err(|error| fail(&resolved.name, error))?;
    Ok(Loaded {
        name: resolved.name,
        pack: Arc::new(pack),
    })
}

/// Starts one arm's host. A host that fails to start fails that arm's runs,
/// not the report.
async fn arm(
    name: dalgona_batteries::eval::report::ArmName,
    config: Result<dal_core::Config, ExecError>,
    roots: &Roots,
    vars: &Vars,
    hosts: &mut Vec<Arc<ArmHost>>,
) -> Arm {
    let started = match config {
        Ok(config) => ArmHost::start(config, roots.data.clone(), vars.clone()).await,
        Err(error) => Err(error),
    };
    let executor = started.map(|host| {
        hosts.push(Arc::clone(&host));
        host as Arc<dyn Executor>
    });
    Arm { name, executor }
}

async fn execute(args: Args, vars: Vars, roots: Roots) -> ExitCode {
    use dalgona_batteries::eval::report::ArmName;
    let loaded = {
        let (requested, roots) = (args.pack.clone(), roots.clone());
        blocking(move || load_pack(&requested, &roots)).await
    };
    let loaded = match loaded {
        Ok(loaded) => loaded,
        Err(code) => return code,
    };
    let user = {
        let path = roots.config_file.clone();
        blocking(move || read_optional(&path)).await
    };
    let inputs = |against: Option<&str>| {
        user.as_ref()
            .map_err(|error| ExecError::Arm(error.to_string().into()))
            .and_then(|user| {
                load_config(&ArmInputs {
                    data_root: &roots.data,
                    user_toml: user.as_deref(),
                    against_toml: against,
                    model: args.model.as_deref(),
                })
            })
    };
    let defaults_config = inputs(None);
    let Some(model) = args.model.clone().or_else(|| {
        defaults_config
            .as_ref()
            .ok()
            .and_then(|config| config.model().map(str::to_owned))
    }) else {
        let reason = match &defaults_config {
            Err(error) => error.to_string(),
            Ok(_) => "no model: pass --model <id> or set `model` in dal.toml".to_owned(),
        };
        return fail(&loaded.name, reason);
    };
    let options = Options {
        pack_name: loaded.name.clone(),
        model,
        against: args.against.as_ref().map(|path| path.display().to_string()),
        case: args.case.clone(),
        repeats: args.repeats,
    };
    if let Err(error) = options.check(&loaded.pack) {
        return fail(&loaded.name, error);
    }
    let mut hosts = Vec::new();
    let mut arms = vec![
        arm(
            ArmName::Defaults,
            defaults_config,
            &roots,
            &vars,
            &mut hosts,
        )
        .await,
    ];
    if let Some(path) = &args.against {
        let text = {
            let path = path.clone();
            blocking(move || read_optional(&path)).await
        };
        let against_config = text
            .and_then(|text| {
                text.ok_or_else(|| {
                    ExecError::Arm(format!("cannot read {}: no such file", path.display()).into())
                })
            })
            .and_then(|text| inputs(Some(&text)));
        arms.push(arm(ArmName::Against, against_config, &roots, &vars, &mut hosts).await);
    }
    let code = drive(&loaded, &options, &arms, &roots).await;
    for host in &hosts {
        host.shutdown().await;
    }
    code
}

#[expect(
    clippy::disallowed_methods,
    reason = "process edge: the eval command owns its interrupt signal"
)]
async fn interrupt() {
    let _ = tokio::signal::ctrl_c().await;
}

/// The files of one run: `report.json` and the append-only `runs.jsonl`.
struct RunDir {
    dir: PathBuf,
    runs: File,
}

impl RunDir {
    fn create(roots: &Roots, pack: &str, run_id: &str) -> std::io::Result<Self> {
        let dir = roots
            .data
            .join("evals")
            .join("runs")
            .join(pack)
            .join(run_id);
        std::fs::create_dir_all(&dir)?;
        let runs = File::options()
            .create(true)
            .append(true)
            .open(dir.join("runs.jsonl"))?;
        Ok(Self { dir, runs })
    }

    fn append(&mut self, line: &RunLine) -> std::io::Result<()> {
        let json = line.to_json().map_err(std::io::Error::other)?;
        writeln!(self.runs, "{json}")?;
        self.runs.flush()
    }

    fn write_report(&self, report: &Report) -> std::io::Result<()> {
        let json = report.to_json().map_err(std::io::Error::other)?;
        let staging = self.dir.join("report.json.tmp");
        std::fs::write(&staging, json)?;
        std::fs::rename(&staging, self.dir.join("report.json"))
    }
}

/// Prints why a run did not pass, so a failure is not only a count.
fn note(line: &RunLine) {
    if line.status == Status::Passed {
        return;
    }
    let reason = line.reason.as_deref().unwrap_or("no reason recorded");
    let _ = writeln!(
        std::io::stderr().lock(),
        "{} run {} ({:?}): {reason}",
        line.case,
        line.repeat,
        line.arm,
    );
}

async fn drive(loaded: &Loaded, options: &Options, arms: &[Arm], roots: &Roots) -> ExitCode {
    let stamp = Stamp::now();
    let created = {
        let (roots, pack, run_id) = (roots.clone(), loaded.name.clone(), stamp.run_id.clone());
        blocking(move || RunDir::create(&roots, &pack, &run_id)).await
    };
    let mut files = match created {
        Ok(files) => files,
        Err(error) => {
            return fail(
                &loaded.name,
                format!("cannot write the run directory: {error}"),
            );
        }
    };
    let cancel = CancellationToken::new();
    let finished = {
        let mut log = |line: &RunLine| {
            tokio::task::block_in_place(|| {
                note(line);
                files.append(line)
            })
        };
        let mut running = std::pin::pin!(run::run(
            &loaded.pack,
            options,
            arms,
            &cancel,
            &stamp.started_at,
            &mut log,
        ));
        let mut signal = std::pin::pin!(interrupt());
        loop {
            tokio::select! {
                result = &mut running => break result,
                () = &mut signal, if !cancel.is_cancelled() => cancel.cancel(),
            }
        }
    };
    let report = match finished {
        Ok(report) => report,
        Err(error) => return fail(&loaded.name, format!("cannot write runs.jsonl: {error}")),
    };
    let interrupted = report.interrupted;
    let written = blocking(move || {
        files.write_report(&report)?;
        let _ = write!(std::io::stdout().lock(), "{}", report.render());
        Ok::<(), std::io::Error>(())
    })
    .await;
    if let Err(error) = written {
        return fail(&loaded.name, format!("cannot write report.json: {error}"));
    }
    if interrupted {
        ExitCode::from(130)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests;
