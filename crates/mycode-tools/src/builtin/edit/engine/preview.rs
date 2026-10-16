//! Line-oriented diff previews and escaped snippets for edit results.

use super::super::{MAX_DIFF_SNIPPET, MAX_DIFF_SUMMARY_BYTES};

pub(super) fn append_diff(diff: &mut String, old: &str, new: &str) {
    if old == new || diff.len() >= MAX_DIFF_SUMMARY_BYTES {
        return;
    }
    let hunk = line_diff_preview(old, new);
    let remaining = MAX_DIFF_SUMMARY_BYTES.saturating_sub(diff.len());
    if hunk.len() > remaining {
        diff.push_str("[diff truncated]\n");
        return;
    }
    diff.push_str(&hunk);
}

/// Line-oriented preview of additions and deletions. Unchanged lines are
/// omitted so the UI can show what actually changed.
fn line_diff_preview(old: &str, new: &str) -> String {
    const MAX_SIDE_LINES: usize = 80;
    const MAX_EMITTED: usize = 40;
    const PREVIEW_LINE_BYTES: usize = 200;
    let old_lines = split_preview_lines(old);
    let new_lines = split_preview_lines(new);
    if old_lines.len() > MAX_SIDE_LINES || new_lines.len() > MAX_SIDE_LINES {
        return truncated_sides(&old_lines, &new_lines, PREVIEW_LINE_BYTES, MAX_EMITTED);
    }
    let mut out = String::new();
    for (emitted, (sign, line)) in changed_lines(&old_lines, &new_lines)
        .into_iter()
        .enumerate()
    {
        if emitted >= MAX_EMITTED {
            out.push_str("[diff truncated]\n");
            break;
        }
        push_preview_line(&mut out, sign, line, PREVIEW_LINE_BYTES);
    }
    out
}

fn split_preview_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

fn changed_lines<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<(char, &'a str)> {
    let n = old.len();
    let m = new.len();
    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1].saturating_add(1)
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    let mut j = 0;
    while i < n && j < m {
        if old[i] == new[j] {
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push(('-', old[i]));
            i += 1;
        } else {
            out.push(('+', new[j]));
            j += 1;
        }
    }
    while i < n {
        out.push(('-', old[i]));
        i += 1;
    }
    while j < m {
        out.push(('+', new[j]));
        j += 1;
    }
    out
}

fn truncated_sides(old: &[&str], new: &[&str], line_bytes: usize, max_emitted: usize) -> String {
    let old_take = max_emitted / 2;
    let shown = old.len().min(old_take);
    let mut out = String::new();
    for line in old.iter().take(old_take) {
        push_preview_line(&mut out, '-', line, line_bytes);
    }
    for line in new.iter().take(max_emitted.saturating_sub(shown)) {
        push_preview_line(&mut out, '+', line, line_bytes);
    }
    out.push_str("[diff truncated]\n");
    out
}

fn push_preview_line(out: &mut String, sign: char, line: &str, line_bytes: usize) {
    let (cut, truncated) = crate::builtin::truncate_bytes(line, line_bytes);
    // One `+` or `-`, then a space. The transcript paints those lines green
    // and red. A doubled sign was showing up as `++` / `--` in the preview.
    out.push(sign);
    out.push(' ');
    out.push_str(&cut.replace('\r', "\\r"));
    if truncated {
        out.push('\u{2026}');
    }
    out.push('\n');
}

pub(crate) fn snippet(text: &str) -> String {
    let (cut, truncated) = crate::builtin::truncate_bytes(text, MAX_DIFF_SNIPPET);
    let mut visible = cut.replace('\r', "\\r").replace('\n', "\\n");
    if truncated {
        visible.push('\u{2026}');
    }
    visible
}

#[cfg(test)]
mod tests {
    use super::line_diff_preview;

    #[test]
    fn changed_lines_use_a_single_sign() {
        let preview = line_diff_preview("alpha\nbeta\n", "alpha\ngamma\n");
        assert!(preview.contains("- beta\n"), "{preview}");
        assert!(preview.contains("+ gamma\n"), "{preview}");
        assert!(!preview.contains("-- "), "{preview}");
        assert!(!preview.contains("++ "), "{preview}");
    }
}
