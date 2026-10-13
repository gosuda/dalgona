// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Prompt rendering after dependencies settle.

use dal_core::JobId;

use super::{POOL_REPORT_LIMIT, PoolItemResult, Step, StepResult};

/// Renders a prompt after all declared dependencies have settled.
pub(crate) fn render(
    step: &Step,
    item: Option<&str>,
    reports: &[StepResult],
    input: Option<&str>,
    run: JobId,
) -> String {
    let mut rendered = String::with_capacity(step.prompt.len());
    let mut offset = 0;
    while let Some(open_rel) = step.prompt[offset..].find("{{") {
        let open = offset + open_rel;
        let Some(close_rel) = step.prompt[open + 2..].find("}}") else {
            rendered.push_str(&step.prompt[offset..]);
            return rendered;
        };
        let close = open + 2 + close_rel;
        rendered.push_str(&step.prompt[offset..open]);
        let placeholder = &step.prompt[open + 2..close];
        if placeholder == "item" {
            rendered.push_str(item.unwrap_or_default());
        } else if placeholder == "input" {
            rendered.push_str(input.unwrap_or_default());
        } else if let Some(name) = placeholder.strip_prefix("step:") {
            if let Some(result) = reports.iter().find(|result| result.name == name) {
                match (&result.task_report, &result.pool_items) {
                    (Some(report), _) => rendered.push_str(report),
                    (None, Some(items)) => {
                        rendered.push_str(&render_pool_report(items, run));
                    }
                    (None, None) => {}
                }
            }
        } else {
            rendered.push_str(&step.prompt[open..close + 2]);
        }
        offset = close + 2;
    }
    rendered.push_str(&step.prompt[offset..]);
    rendered
}

fn item_line(item: &PoolItemResult) -> String {
    let mut line = String::with_capacity(
        2 + truncate_bytes(&item.item, 80).len() + 2 + item.state.len() + 2 + item.summary.len(),
    );
    line.push_str("- ");
    line.push_str(truncate_bytes(&item.item, 80));
    line.push_str(": ");
    line.push_str(&item.state);
    line.push_str(": ");
    line.push_str(&item.summary);
    line
}

fn render_pool_report(items: &[PoolItemResult], run: JobId) -> String {
    let total = items
        .iter()
        .enumerate()
        .fold(0_usize, |total, (index, item)| {
            let prefix = usize::from(index != 0);
            total
                .saturating_add(prefix)
                .saturating_add(item_line(item).len())
        });
    if total <= POOL_REPORT_LIMIT {
        return items.iter().map(item_line).collect::<Vec<_>>().join("\n");
    }
    let suffix = format!("\n(cut; read job://{run} for every item)");
    let content_limit = POOL_REPORT_LIMIT.saturating_sub(suffix.len());
    let mut rendered = String::with_capacity(POOL_REPORT_LIMIT);
    for (index, item) in items.iter().enumerate() {
        if index != 0 && !append_bounded(&mut rendered, "\n", content_limit) {
            break;
        }
        if !append_bounded(&mut rendered, &item_line(item), content_limit) {
            break;
        }
    }
    rendered.push_str(&suffix);
    rendered
}

fn append_bounded(output: &mut String, text: &str, limit: usize) -> bool {
    let available = limit.saturating_sub(output.len());
    if text.len() <= available {
        output.push_str(text);
        true
    } else {
        output.push_str(truncate_bytes(text, available));
        false
    }
}

fn truncate_bytes(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
