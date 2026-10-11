//! Shell completion script generation for the single command tree.

use super::{command_for, tree::ShellArg};

pub(crate) fn completion_script(shell: ShellArg, binary: &'static str) -> String {
    use clap_complete::{generate, shells};
    use std::io::Cursor;

    let mut command = command_for(binary);
    let mut output = Cursor::new(Vec::new());
    match shell {
        ShellArg::Bash => generate(shells::Bash, &mut command, binary, &mut output),
        ShellArg::Elvish => generate(shells::Elvish, &mut command, binary, &mut output),
        ShellArg::Fish => generate(shells::Fish, &mut command, binary, &mut output),
        ShellArg::Powershell => generate(shells::PowerShell, &mut command, binary, &mut output),
        ShellArg::Zsh => generate(shells::Zsh, &mut command, binary, &mut output),
    }
    String::from_utf8(output.into_inner()).unwrap_or_default()
}
