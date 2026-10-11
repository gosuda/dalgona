//! Fixed-width status slots whose meaning remains textual without color.

use crate::width::{WidthMode, take_cells, width};

/// Cells reserved for the spinner and state word.
const STATE_SLOT: usize = 12;

/// Values displayed by one status row.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusData<'a> {
    /// Current state word; idle leaves this empty.
    pub state: Option<&'a str>,
    /// Braille spinner cell, frozen on its first frame under reduced motion.
    pub spinner: Option<&'a str>,
    /// Model identifier.
    pub model: Option<&'a str>,
    /// Workspace path and branch.
    pub path: Option<&'a str>,
    /// Input and output token labels.
    pub tokens: Option<&'a str>,
    /// Context usage percentage.
    pub context: Option<&'a str>,
    /// Role carried by the context slot when its word warns or errors.
    pub context_role: Option<crate::theme::Role>,
    /// Agent or job count.
    pub agents: Option<&'a str>,
    /// Session cost.
    pub cost: Option<&'a str>,
}

/// Context usage at or above this share of the window warns.
pub(crate) const CONTEXT_RISING_AT: u64 = 70;
/// Context usage at or above this share of the window errors.
pub(crate) const CONTEXT_HIGH_AT: u64 = 90;

/// Renders the context slot for one usage percentage: the copy-deck text plus
/// the role the slot carries. The word carries the state, never color alone.
#[must_use]
pub(crate) fn context_slot(percent: u64) -> (String, Option<crate::theme::Role>) {
    use crate::copy;
    use crate::theme::Role;
    let (template, role) = if percent >= CONTEXT_HIGH_AT {
        (copy::ids::STATUS_CONTEXT_HIGH, Some(Role::Error))
    } else if percent >= CONTEXT_RISING_AT {
        (copy::ids::STATUS_CONTEXT_RISING, Some(Role::Warning))
    } else {
        (copy::ids::STATUS_CONTEXT, None)
    };
    (
        copy::render(template, &[("pct", &percent.to_string())], 1),
        role,
    )
}

/// Renders status segments for a terminal width.
///
/// Numeric fields retain fixed slot widths and right alignment. Segments drop
/// from the right according to the width bands in the TUI design contract.
#[must_use]
pub fn render(data: StatusData<'_>, columns: usize, mode: WidthMode) -> String {
    render_with_roles(data, columns, mode).0
}

/// Like [`render`], and also returns the byte range and role of every segment
/// that carries one, so the row can style the context word without color alone.
#[must_use]
pub fn render_with_roles(
    data: StatusData<'_>,
    columns: usize,
    mode: WidthMode,
) -> (String, Vec<(std::ops::Range<usize>, crate::theme::Role)>) {
    let state = state_slot(data.spinner, data.state);
    // The state word never truncates: its slot widens to fit the longest word.
    let state_width = state
        .as_deref()
        .map_or(STATE_SLOT, |text| width(text, mode).max(STATE_SLOT));
    // The context slot widens the same way to keep its state word readable.
    let context_width = data
        .context
        .map_or(CONTEXT_SLOT, |text| width(text, mode).max(CONTEXT_SLOT));
    let compact_tokens = data.tokens.map(compact_token_pair);
    let basename = data.path.map(path_basename);
    let slots = slots_for(
        &data,
        state.as_deref(),
        state_width,
        context_width,
        compact_tokens.as_deref(),
        basename.as_deref(),
        columns,
    );

    let mut segments: Vec<String> = Vec::new();
    let mut spans = Vec::new();
    let mut offset = 0;
    for slot in &slots {
        let Some(value) = slot.value else {
            continue;
        };
        let Some(segment) = fit(value, slot.cells, slot.align, columns, &segments, mode) else {
            continue;
        };
        if let Some(role) = slot.role {
            spans.push((offset..offset + segment.len(), role));
        }
        offset += segment.len() + SEPARATOR.len();
        segments.push(segment);
    }
    // Below twenty columns the state slot may itself drop; the model then
    // shows truncated in its place, keeping the row from collapsing.
    if segments.is_empty()
        && columns < 20
        && let Some(model) = data.model
    {
        let state_cells = width(state.as_deref().unwrap_or(""), mode);
        let available = columns.saturating_sub(state_cells + 1);
        if available > 0 {
            segments.push(format!(
                "...{}",
                take_cells(model, available.saturating_sub(3), mode)
            ));
        }
    }
    let line = segments.join(SEPARATOR);
    (take_cells(&line, columns, mode), spans)
}

/// The slot layout for one terminal width band. Segments drop whole from the
/// right, and the context slot carries its role through every band.
fn slots_for<'a>(
    data: &StatusData<'a>,
    state: Option<&'a str>,
    state_width: usize,
    context_width: usize,
    compact_tokens: Option<&'a str>,
    basename: Option<&'a str>,
    columns: usize,
) -> Vec<Slot<'a>> {
    let state = Slot {
        value: state,
        cells: state_width,
        align: Align::Left,
        role: None,
    };
    let context = Slot {
        value: data.context,
        cells: context_width,
        align: Align::Right,
        role: data.context_role,
    };
    let model = Slot::fixed(data.model, 24, Align::Left);
    let path = Slot::fixed(data.path, 24, Align::Left);
    if columns >= 120 {
        vec![
            state,
            model,
            path,
            Slot::fixed(data.tokens, 13, Align::Right),
            context,
            Slot::fixed(data.agents, 10, Align::Right),
            Slot::fixed(data.cost, 7, Align::Right),
        ]
    } else if columns >= 80 {
        vec![
            state,
            model,
            path,
            Slot::fixed(data.tokens, 13, Align::Right),
            context,
            Slot::fixed(data.agents, 10, Align::Right),
        ]
    } else if columns >= 60 {
        vec![
            state,
            model,
            path,
            Slot {
                value: compact_tokens,
                cells: 13,
                align: Align::Right,
                role: None,
            },
            context,
            Slot::fixed(data.agents, 10, Align::Right),
        ]
    } else if columns >= 40 {
        vec![
            state,
            model,
            Slot {
                value: basename,
                cells: 24,
                align: Align::Left,
                role: None,
            },
            context,
        ]
    } else {
        vec![state, context]
    }
}

/// Horizontal placement of one status segment inside its slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    /// Padded on the right.
    Left,
    /// Padded on the left.
    Right,
}

/// One status segment request: value, reserved cells, alignment, carried role.
struct Slot<'a> {
    value: Option<&'a str>,
    cells: usize,
    align: Align,
    role: Option<crate::theme::Role>,
}

impl Slot<'_> {
    /// A slot that never carries a role.
    fn fixed(value: Option<&str>, cells: usize, align: Align) -> Slot<'_> {
        Slot {
            value,
            cells,
            align,
            role: None,
        }
    }
}

/// Cells reserved for the context percentage.
const CONTEXT_SLOT: usize = 8;

/// Writes `path` with a leading `~` when it sits at or under `home`.
#[must_use]
pub fn contract_home(path: &str, home: Option<&str>) -> String {
    let Some(home) = home.map(|home| home.trim_end_matches('/')) else {
        return path.to_owned();
    };
    if home.is_empty() {
        return path.to_owned();
    }
    match path.strip_prefix(home) {
        Some("") => "~".to_owned(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

/// The branch checked out in the repository holding `workspace`, or the short object
/// name of a detached head. Reads `HEAD` directly, so a linked worktree (a `.git`
/// file naming its git directory) answers as well. `None` outside a repository.
#[must_use]
pub(crate) fn git_branch(workspace: &std::path::Path) -> Option<String> {
    for directory in workspace.ancestors() {
        let dot_git = directory.join(".git");
        let git_dir = if dot_git.is_dir() {
            dot_git
        } else if let Ok(pointer) = std::fs::read_to_string(&dot_git) {
            directory.join(pointer.trim().strip_prefix("gitdir:")?.trim())
        } else {
            continue;
        };
        let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
        let head = head.trim();
        return match head.strip_prefix("ref:") {
            Some(reference) => {
                let reference = reference.trim();
                Some(
                    reference
                        .strip_prefix("refs/heads/")
                        .unwrap_or(reference)
                        .to_owned(),
                )
            }
            None if head.len() >= 7 && head.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
                Some(head[..7].to_owned())
            }
            None => None,
        };
    }
    None
}

/// The workspace branch a client may show: the local read only when the
/// workspace lives on this machine's disk. A remote host's workspace path
/// names the host's disk, so the branch hides instead of risking wrong data.
#[must_use]
pub(crate) fn workspace_branch(
    local_workspace: bool,
    workspace: &std::path::Path,
) -> Option<String> {
    if !local_workspace {
        return None;
    }
    git_branch(workspace)
}

fn state_slot(spinner: Option<&str>, state: Option<&str>) -> Option<String> {
    match (spinner, state) {
        (Some(spinner), Some(state)) => Some(format!("{spinner} {state}")),
        (None, Some(state)) => Some(state.to_owned()),
        (Some(spinner), None) => Some(spinner.to_owned()),
        (None, None) => None,
    }
}

fn fit(
    value: &str,
    slot_width: usize,
    align: Align,
    columns: usize,
    placed: &[String],
    mode: WidthMode,
) -> Option<String> {
    let separators = placed.len().saturating_mul(width(SEPARATOR, mode));
    let used = separators
        + placed
            .iter()
            .map(|segment| width(segment, mode))
            .sum::<usize>();
    let remaining = columns.saturating_sub(used);
    if remaining == 0 {
        return None;
    }
    let allotted = slot_width.min(remaining);
    let text = take_cells(value, allotted, mode);
    let padding = allotted.saturating_sub(width(&text, mode));
    Some(match align {
        Align::Left => format!("{text}{}", " ".repeat(padding)),
        Align::Right => format!("{}{text}", " ".repeat(padding)),
    })
}

/// The ` · ` segment separator: the one measured Ambiguous chrome glyph.
const SEPARATOR: &str = " · ";

/// Turns `in {in} out {out}` into the 60-79 column form `{in}/{out}`.
fn compact_token_pair(value: &str) -> String {
    match value.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["in", input, "out", output] => format!("{input}/{output}"),
        _ => value.to_owned(),
    }
}

/// Builds the SGR prefix for one theme role: the role color at the resolved
/// depth plus its modifier, so meaning survives `NO_COLOR`.
#[must_use]
pub fn role_sgr(theme: &crate::theme::ResolvedTheme, role: crate::theme::Role) -> String {
    style_sgr(theme, role, false)
}

/// Like [`role_sgr`], with bold added when `bold` is set; bold survives `NO_COLOR`.
#[must_use]
pub(crate) fn style_sgr(
    theme: &crate::theme::ResolvedTheme,
    role: crate::theme::Role,
    bold: bool,
) -> String {
    use ratatui::style::{Color, Modifier};
    let mut params: Vec<String> = Vec::new();
    if bold {
        params.push("1".to_owned());
    }
    if crate::theme::ResolvedTheme::modifiers(role).contains(Modifier::DIM) {
        params.push("2".to_owned());
    }
    let named = |code: u8| Some(code.to_string());
    let foreground = match theme.color(role) {
        Color::Black => named(30),
        Color::Red => named(31),
        Color::Green => named(32),
        Color::Yellow => named(33),
        Color::Blue => named(34),
        Color::Magenta => named(35),
        Color::Cyan => named(36),
        Color::Gray => named(37),
        Color::DarkGray => named(90),
        Color::LightRed => named(91),
        Color::LightGreen => named(92),
        Color::LightYellow => named(93),
        Color::LightBlue => named(94),
        Color::LightMagenta => named(95),
        Color::LightCyan => named(96),
        Color::White => named(97),
        Color::Rgb(red, green, blue) => Some(format!("38;2;{red};{green};{blue}")),
        Color::Indexed(index) => Some(format!("38;5;{index}")),
        Color::Reset => None,
    };
    params.extend(foreground);
    if params.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", params.join(";"))
    }
}

fn path_basename(path: &str) -> String {
    let (path, branch) = path
        .split_once(" ( ")
        .map_or((path, None), |(path, rest)| (path, Some(rest)));
    let basename = path.rsplit(['/', '\\']).next().unwrap_or(path);
    branch.map_or_else(
        || basename.to_owned(),
        |branch| format!("{basename} ({branch}"),
    )
}

#[cfg(test)]
mod tests {
    use super::{StatusData, render, role_sgr};
    use crate::theme::Role;
    use crate::width::WidthMode;
    use crate::{ColorMode, ThemeRequest};

    #[test]
    fn role_sgr_follows_color_depth_and_no_color() {
        let truecolor = crate::theme::load(
            &ThemeRequest::Named("flexoki-dark".into()),
            ColorMode::Truecolor,
            None,
            None,
        )
        .expect("shipped theme loads");
        let dim = role_sgr(&truecolor, Role::Dim);
        assert!(dim.starts_with("\x1b[2;38;2;"), "{dim:?}");
        let never = crate::theme::load(&ThemeRequest::Palette, ColorMode::Never, None, None)
            .expect("palette loads");
        assert_eq!(role_sgr(&never, Role::Dim), "\x1b[2m");
        assert_eq!(role_sgr(&never, Role::Text), "");
    }

    #[test]
    fn bands_keep_status_state_and_drop_whole_right_slots() {
        let data = StatusData {
            state: Some("working"),
            spinner: Some("⠋"),
            model: Some("provider/model"),
            path: Some("~/work/project (main)"),
            tokens: Some("in 14k out 4k"),
            context: Some("ctx 47%"),
            context_role: None,
            agents: Some("3 agents"),
            cost: Some("$0.42"),
        };
        let full = render(data, 120, WidthMode::Narrow);
        assert!(full.contains("$0.42"));
        let medium = render(data, 30, WidthMode::Narrow);
        assert!(medium.contains("working"));
        let narrow = render(data, 15, WidthMode::Narrow);
        assert!(narrow.contains("working"));
        assert!(!narrow.contains("provider/model"));
    }

    #[test]
    fn waiting_state_word_is_never_truncated() {
        let data = StatusData {
            state: Some("waiting for you"),
            model: Some("provider/model"),
            ..StatusData::default()
        };
        for columns in [40, 80, 120] {
            let line = render(data, columns, WidthMode::Narrow);
            assert!(
                line.starts_with("waiting for you"),
                "{columns} columns cut the state word: {line:?}"
            );
        }
    }

    #[test]
    fn home_contraction_writes_tilde_only_on_a_path_boundary() {
        use super::contract_home;

        assert_eq!(contract_home("/home/ada/work", Some("/home/ada")), "~/work");
        assert_eq!(contract_home("/home/ada", Some("/home/ada/")), "~");
        assert_eq!(
            contract_home("/home/adam/work", Some("/home/ada")),
            "/home/adam/work"
        );
        assert_eq!(contract_home("/srv/work", Some("/home/ada")), "/srv/work");
        assert_eq!(contract_home("/srv/work", None), "/srv/work");
        assert_eq!(contract_home("/srv/work", Some("")), "/srv/work");
    }

    #[test]
    fn context_slot_is_kept_in_the_twenty_column_band() {
        let rendered = render(
            StatusData {
                state: Some("working"),
                spinner: Some("*"),
                context: Some("ctx 47%"),
                ..StatusData::default()
            },
            30,
            WidthMode::Narrow,
        );
        assert!(rendered.contains("ctx 47%"));
    }

    #[test]
    fn context_words_and_roles_follow_the_seventy_and_ninety_percent_bands() {
        use super::context_slot;
        use crate::theme::Role;

        let (plain, role) = context_slot(69);
        assert_eq!(
            plain, "ctx 69%",
            "below the warn band the slot carries no word"
        );
        assert_eq!(role, None);
        let (rising, role) = context_slot(70);
        assert_eq!(rising, "ctx 70% · rising");
        assert_eq!(role, Some(Role::Warning));
        let (_, role) = context_slot(89);
        assert_eq!(role, Some(Role::Warning));
        let (high, role) = context_slot(90);
        assert_eq!(high, "ctx 90% · high");
        assert_eq!(role, Some(Role::Error));
    }

    #[test]
    fn a_rising_context_slot_keeps_its_word_inside_the_fixed_bands() {
        use crate::theme::Role;

        let data = StatusData {
            context: Some("ctx 71% · rising"),
            context_role: Some(Role::Warning),
            ..StatusData::default()
        };
        for columns in [40, 70, 80, 120] {
            let (line, spans) = super::render_with_roles(data, columns, WidthMode::Narrow);
            assert!(
                line.contains("ctx 71% · rising"),
                "{columns} columns truncates the context word: {line:?}"
            );
            let (range, role) = spans.first().expect("the context span is present").clone();
            assert_eq!(role, Role::Warning);
            assert_eq!(&line[range.clone()], "ctx 71% · rising");
            assert_eq!(range.start, line.find("ctx 71%").expect("ctx slot start"));
        }
    }

    #[test]
    fn a_high_context_slot_carries_the_error_role_in_the_row() {
        use crate::theme::Role;

        let data = StatusData {
            state: Some("working"),
            spinner: Some("⠋"),
            context: Some("ctx 91% · high"),
            context_role: Some(Role::Error),
            ..StatusData::default()
        };
        let (line, spans) = super::render_with_roles(data, 80, WidthMode::Narrow);
        assert!(line.contains("ctx 91% · high"), "{line:?}");
        assert!(
            spans.iter().any(|(_, role)| *role == Role::Error),
            "the high slot carries the error role: {spans:?}"
        );
    }

    #[test]
    fn the_branch_hides_when_the_workspace_is_not_this_machine_s_disk() {
        use super::workspace_branch;
        let root = tempfile::tempdir().expect("temp dir");
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/feature/x\n").expect("HEAD");

        assert_eq!(
            workspace_branch(true, &repo).as_deref(),
            Some("feature/x"),
            "a local workspace shows its branch"
        );
        assert_eq!(
            workspace_branch(false, &repo),
            None,
            "a remote workspace path names the host's disk; the branch hides"
        );
    }

    #[test]
    fn sixty_to_seventy_nine_columns_show_tokens_as_a_slash_pair() {
        let data = StatusData {
            model: Some("m"),
            tokens: Some("in 14k out 4k"),
            ..StatusData::default()
        };
        let line = render(data, 70, WidthMode::Narrow);
        assert!(line.contains("14k/4k"), "{line:?}");
        assert!(!line.contains("in/"), "{line:?}");
        assert!(render(data, 90, WidthMode::Narrow).contains("in 14k out 4k"));
    }

    #[test]
    fn git_branch_reads_branch_detached_head_and_linked_worktrees() {
        use super::git_branch;
        let root = tempfile::tempdir().expect("temp dir");
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        std::fs::create_dir_all(repo.join("src/deep")).expect("nested dir");
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/feature/x\n").expect("HEAD");
        assert_eq!(git_branch(&repo).as_deref(), Some("feature/x"));
        assert_eq!(
            git_branch(&repo.join("src/deep")).as_deref(),
            Some("feature/x")
        );

        std::fs::write(
            repo.join(".git/HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .expect("detached HEAD");
        assert_eq!(git_branch(&repo).as_deref(), Some("0123456"));

        let linked = root.path().join("linked");
        let admin = repo.join(".git/worktrees/linked");
        std::fs::create_dir_all(&linked).expect("worktree dir");
        std::fs::create_dir_all(&admin).expect("admin dir");
        std::fs::write(admin.join("HEAD"), "ref: refs/heads/topic\n").expect("linked HEAD");
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .expect("gitdir pointer");
        assert_eq!(git_branch(&linked).as_deref(), Some("topic"));

        std::fs::write(repo.join(".git/HEAD"), "garbage").expect("broken HEAD");
        assert_eq!(git_branch(&repo), None);
    }

    #[test]
    fn narrow_mode_budget_counts_separator_cells() {
        let line = render(
            StatusData {
                state: Some("s"),
                model: Some("m"),
                path: Some("p"),
                tokens: Some("in 14k out 4k"),
                ..StatusData::default()
            },
            70,
            WidthMode::Narrow,
        );
        assert!(
            line.ends_with('1'),
            "the fourth slot receives its one remaining cell: {line:?}"
        );
    }
}
