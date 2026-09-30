/// Classifies one shell command as read-only for ordered tool dispatch.
///
/// Classification is intentionally conservative: shell syntax the parser cannot
/// prove safe stays in a serial, approval-requiring unit. `cwd` is reserved for
/// the approval layer's root checks; this function is cwd-independent.
#[must_use]
pub fn exec_reads_only(command: &str, _cwd: Option<&str>) -> bool {
    if command.contains('\0')
        || command.contains('\n')
        || command.contains('\r')
        || FORBIDDEN.iter().any(|operator| command.contains(operator))
    {
        return false;
    }

    let mut tokens = Tokens {
        source: command,
        pos: 0,
    };
    let Ok(Some(first)) = tokens.next() else {
        return false;
    };
    if is_assignment(first) || !READ_ONLY_FIRST_WORDS.contains(&first) {
        return false;
    }

    match first {
        "find" => guards::find_ok(&mut tokens),
        "git" => guards::git_ok(&mut tokens),
        "cargo" => guards::cargo_ok(&mut tokens),
        "env" => guards::env_ok(&mut tokens),
        "fd" => guards::fd_ok(&mut tokens),
        "rg" => guards::rg_ok(&mut tokens),
        _ => tokens.finish(),
    }
}

const READ_ONLY_FIRST_WORDS: &[&str] = &[
    "cat", "head", "tail", "wc", "file", "stat", "du", "df", "ls", "tree", "which", "pwd", "rg",
    "grep", "fd", "git", "cargo", "find", "env",
];

/// Shell operators are rejected even inside quotes. This avoids treating a quoted
/// separator differently from the shell parser that will execute the command.
const FORBIDDEN: &[&str] = &["$(", "`", "&&", "||", ";", "|", ">", "<", "&"];

#[derive(Clone, Copy, Debug)]
enum TokenizeError {
    TrailingEscape,
    UnterminatedQuote,
}

#[derive(Clone, Copy)]
struct Tokens<'a> {
    source: &'a str,
    pos: usize,
}

impl<'a> Tokens<'a> {
    fn next(&mut self) -> Result<Option<&'a str>, TokenizeError> {
        let bytes = self.source.as_bytes();
        let mut i = self.pos;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == bytes.len() {
            self.pos = i;
            return Ok(None);
        }

        let start = i;
        let mut quote = 0u8;
        while i < bytes.len() {
            let byte = bytes[i];
            if byte == b'\\' && quote != b'\'' {
                i += 1;
                if i == bytes.len() {
                    return Err(TokenizeError::TrailingEscape);
                }
                i += 1;
                continue;
            }
            match (quote, byte) {
                (0, b'\'' | b'"') => quote = byte,
                (b'\'', b'\'') | (b'"', b'"') => quote = 0,
                (0, whitespace) if whitespace.is_ascii_whitespace() => break,
                _ => {}
            }
            i += 1;
        }
        if quote != 0 {
            return Err(TokenizeError::UnterminatedQuote);
        }
        self.pos = i;
        Ok(Some(&self.source[start..i]))
    }

    fn finish(&mut self) -> bool {
        loop {
            match self.next() {
                Ok(Some(_)) => {}
                Ok(None) => return true,
                Err(_) => return false,
            }
        }
    }
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Clone, Copy)]
enum WordMatch {
    Exact,
    Prefix,
}

/// Compares a token with a shell word without allocating. Quotes and escapes
/// are removed only for checks where the shell sees the same decoded argument.
fn shell_word_eq(raw: &str, expected: &str) -> bool {
    shell_word_matches(raw, expected, WordMatch::Exact)
}

fn shell_word_starts_with(raw: &str, prefix: &str) -> bool {
    shell_word_matches(raw, prefix, WordMatch::Prefix)
}

fn shell_word_matches(raw: &str, expected: &str, mode: WordMatch) -> bool {
    let bytes = raw.as_bytes();
    let expected = expected.as_bytes();
    let mut i = 0;
    let mut matched = 0;
    let mut quote = 0u8;

    while i < bytes.len() {
        let decoded = decode_word_byte(bytes, &mut i, &mut quote);
        match decoded {
            ShellWordByte::Literal(byte) => {
                if !matches_expected(byte, expected, &mut matched) {
                    return false;
                }
                if matched == expected.len() && matches!(mode, WordMatch::Prefix) {
                    return true;
                }
            }
            ShellWordByte::Delimiter => {}
            ShellWordByte::Invalid => return false,
        }
        i += 1;
    }

    quote == 0 && matched == expected.len()
}

#[derive(Clone, Copy)]
enum ShellWordByte {
    Delimiter,
    Literal(u8),
    Invalid,
}

fn decode_word_byte(bytes: &[u8], index: &mut usize, quote: &mut u8) -> ShellWordByte {
    let byte = bytes[*index];
    if *quote == b'\'' {
        if byte == b'\'' {
            *quote = 0;
            return ShellWordByte::Delimiter;
        }
        return ShellWordByte::Literal(byte);
    }
    if byte == b'\\' {
        *index += 1;
        let Some(&escaped) = bytes.get(*index) else {
            return ShellWordByte::Invalid;
        };
        if *quote == 0 && escaped == b'\n' {
            return ShellWordByte::Delimiter;
        }
        return ShellWordByte::Literal(escaped);
    }
    if *quote == b'"' {
        if byte == b'"' {
            *quote = 0;
            return ShellWordByte::Delimiter;
        }
        if matches!(byte, b'$' | b'`') {
            return ShellWordByte::Invalid;
        }
        return ShellWordByte::Literal(byte);
    }
    match byte {
        b'\'' | b'"' => {
            *quote = byte;
            ShellWordByte::Delimiter
        }
        b'$' | b'`' => ShellWordByte::Invalid,
        literal => ShellWordByte::Literal(literal),
    }
}

fn matches_expected(byte: u8, expected: &[u8], matched: &mut usize) -> bool {
    if expected.get(*matched) != Some(&byte) {
        return false;
    }
    *matched += 1;
    true
}

fn has_shell_expansion_or_glob(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let mut i = 0;
    let mut quote = 0u8;

    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b'\\' && quote != b'\'' {
            i += 2;
            continue;
        }
        match (quote, byte) {
            (b'\'', b'\'') | (b'"', b'"') => quote = 0,
            (0, b'\'' | b'"') => quote = byte,
            (b'\'', _) => {}
            (0 | b'"', b'$' | b'`' | b'*' | b'?' | b'[' | b'{' | b'}' | b'~') => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

mod guards {
    const FIND_DENIED: &[&str] = &[
        "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf", "-fls",
    ];
    const GIT_SUBCOMMANDS: &[&str] = &[
        "status",
        "log",
        "diff",
        "show",
        "branch",
        "tag",
        "rev-parse",
        "describe",
        "shortlog",
        "reflog",
        "ls-files",
        "ls-remote",
        "blame",
        "grep",
        "worktree",
        "stash",
        "remote",
        "config",
    ];
    const GIT_BRANCH_FLAGS: &[&str] = &[
        "-a",
        "-r",
        "-l",
        "--list",
        "-v",
        "-vv",
        "-i",
        "--ignore-case",
        "--contains",
        "--no-contains",
        "--merged",
        "--no-merged",
        "--points-at",
        "--format",
        "--sort",
        "--color",
        "--columns",
        "--show-current",
    ];
    const GIT_TAG_FLAGS: &[&str] = &[
        "-l",
        "--list",
        "-n",
        "-i",
        "--ignore-case",
        "--contains",
        "--no-contains",
        "--merged",
        "--no-merged",
        "--points-at",
        "--format",
        "--sort",
        "--color",
        "--column",
    ];
    const GIT_STASH_WORDS: &[&str] = &["list", "show", "grep"];
    const GIT_REMOTE_WORDS: &[&str] = &["-v", "show", "get-url"];
    const CARGO_SUBCOMMANDS: &[&str] = &["metadata", "tree"];
    const FD_EXEC_WORDS: &[&str] = &["-x", "-X", "--exec", "--exec-batch"];

    pub(super) fn find_ok(tokens: &mut super::Tokens<'_>) -> bool {
        loop {
            let argument = match tokens.next() {
                Ok(Some(argument)) => argument,
                Ok(None) => return true,
                Err(_) => return false,
            };
            if super::has_shell_expansion_or_glob(argument)
                || FIND_DENIED
                    .iter()
                    .any(|denied| super::shell_word_eq(argument, denied))
            {
                return false;
            }
        }
    }

    pub(super) fn git_ok(tokens: &mut super::Tokens<'_>) -> bool {
        let second = match tokens.next() {
            Ok(Some(second)) => second,
            Ok(None) => return true,
            Err(_) => return false,
        };
        if second == "-c" || !GIT_SUBCOMMANDS.contains(&second) {
            return false;
        }
        if !git_args_ok(*tokens) {
            return false;
        }
        match second {
            "branch" => branch_or_tag_ok(tokens, GIT_BRANCH_FLAGS),
            "tag" => branch_or_tag_ok(tokens, GIT_TAG_FLAGS),
            "stash" => stash_ok(tokens),
            "remote" => tokens
                .next()
                .is_ok_and(|word| word.is_none_or(|word| GIT_REMOTE_WORDS.contains(&word))),
            "worktree" => tokens
                .next()
                .is_ok_and(|word| word.is_some_and(|word| word == "list")),
            "config" => tokens.next().is_ok_and(|word| {
                word.is_some_and(|arg| arg.starts_with("--get") || arg == "--list")
            }),
            _ => true,
        }
    }

    fn git_args_ok(mut tokens: super::Tokens<'_>) -> bool {
        loop {
            let argument = match tokens.next() {
                Ok(Some(argument)) => argument,
                Ok(None) => return true,
                Err(_) => return false,
            };
            if super::has_shell_expansion_or_glob(argument)
                || super::shell_word_starts_with(argument, "--output")
                || super::shell_word_starts_with(argument, "-o")
            {
                return false;
            }
        }
    }

    fn branch_or_tag_ok(tokens: &mut super::Tokens<'_>, flags: &[&str]) -> bool {
        let mut has_list_flag = false;
        let mut has_operand = false;
        loop {
            let argument = match tokens.next() {
                Ok(Some(argument)) => argument,
                Ok(None) => return has_list_flag || !has_operand,
                Err(_) => return false,
            };
            if !flags.contains(&argument) {
                return false;
            }
            has_list_flag |= argument == "-l" || argument == "--list";
            has_operand |= !argument.starts_with('-');
        }
    }

    fn stash_ok(tokens: &mut super::Tokens<'_>) -> bool {
        let first = match tokens.next() {
            Ok(Some(first)) => first,
            Ok(None) | Err(_) => return false,
        };
        if !GIT_STASH_WORDS.contains(&first) {
            return false;
        }
        loop {
            match tokens.next() {
                Ok(Some(argument)) if argument.starts_with('-') => {}
                Ok(None) => return true,
                _ => return false,
            }
        }
    }

    pub(super) fn cargo_ok(tokens: &mut super::Tokens<'_>) -> bool {
        let Ok(Some(subcommand)) = tokens.next() else {
            return false;
        };
        CARGO_SUBCOMMANDS.contains(&subcommand) && tokens.finish()
    }

    pub(super) fn env_ok(tokens: &mut super::Tokens<'_>) -> bool {
        loop {
            match tokens.next() {
                Ok(Some("-i")) => {}
                Ok(Some("-u")) => match tokens.next() {
                    Ok(Some(_) | None) => {}
                    Err(_) => return false,
                },
                Ok(None) => return true,
                Ok(Some(_)) | Err(_) => return false,
            }
        }
    }

    pub(super) fn fd_ok(tokens: &mut super::Tokens<'_>) -> bool {
        loop {
            let argument = match tokens.next() {
                Ok(Some(argument)) => argument,
                Ok(None) => return true,
                Err(_) => return false,
            };
            if super::has_shell_expansion_or_glob(argument)
                || FD_EXEC_WORDS
                    .iter()
                    .any(|word| super::shell_word_eq(argument, word))
                || super::shell_word_starts_with(argument, "-x")
                || super::shell_word_starts_with(argument, "-X")
                || super::shell_word_starts_with(argument, "--exec=")
                || super::shell_word_starts_with(argument, "--exec-batch=")
            {
                return false;
            }
        }
    }

    pub(super) fn rg_ok(tokens: &mut super::Tokens<'_>) -> bool {
        loop {
            let argument = match tokens.next() {
                Ok(Some(argument)) => argument,
                Ok(None) => return true,
                Err(_) => return false,
            };
            if super::has_shell_expansion_or_glob(argument)
                || super::shell_word_eq(argument, "--pre")
                || super::shell_word_starts_with(argument, "--pre=")
            {
                return false;
            }
        }
    }
}
#[cfg(test)]
mod tests;
