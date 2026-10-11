//! Render CLI man pages for the release workflow.
use std::{env, fs, io, path::Path, process::ExitCode};

fn main() -> ExitCode {
    let Some(out) = env::args().nth(1) else {
        eprintln!("usage: cargo run -p dalgon --example render-man -- <out-dir>");
        return ExitCode::from(2);
    };
    let out = Path::new(&out);
    let mut command = dalgon::cli::command();
    command.build();
    match fs::create_dir_all(out).and_then(|()| render(&command, "", out)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("render-man failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn render(cmd: &clap::Command, prefix: &str, out: &Path) -> io::Result<()> {
    let page = if prefix.is_empty() {
        cmd.get_name().to_owned()
    } else {
        format!("{prefix}-{}", cmd.get_name())
    };
    let mut roff = Vec::new();
    clap_mangen::Man::new(cmd.clone().name(page.clone())).render(&mut roff)?;
    fs::write(out.join(format!("{page}.1")), roff)?;
    for sub in cmd.get_subcommands() {
        render(sub, &page, out)?;
    }
    Ok(())
}
