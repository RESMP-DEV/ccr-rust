// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod cargo;
pub mod git;
pub mod grep;
pub mod npm;
pub mod test_runner;

use lazy_static::lazy_static;
use regex::Regex;
use tracing::debug;

lazy_static! {
    static ref ANSI_RE: Regex = Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap();
    static ref BLANK_LINES_RE: Regex = Regex::new(r"\n{3,}").unwrap();
    static ref PROGRESS_BAR_RE: Regex = Regex::new(r"[^\n]*\r[^\n]*").unwrap();
}

/// Strip ANSI escape sequences from text.
pub fn strip_ansi(text: &str) -> String {
    ANSI_RE.replace_all(text, "").into_owned()
}

/// Collapse runs of 3+ blank lines down to a single blank line.
pub fn collapse_blank_lines(text: &str) -> String {
    BLANK_LINES_RE.replace_all(text, "\n\n").into_owned()
}

/// Remove carriage-return progress bar lines.
pub fn strip_progress_bars(text: &str) -> String {
    PROGRESS_BAR_RE.replace_all(text, "").into_owned()
}

/// Apply all pattern-based compressions to text.
/// Returns the cleaned/compressed text.
pub fn compress(text: &str) -> String {
    debug!(
        input_len = text.len(),
        "output compress: applying pattern compression"
    );
    // Clean first
    let cleaned = strip_ansi(text);
    let cleaned = collapse_blank_lines(&cleaned);
    let cleaned = strip_progress_bars(&cleaned);

    // Try command-specific patterns (return first match)
    if let Some(result) = git::try_compress(&cleaned) {
        return result;
    }
    if let Some(result) = cargo::try_compress(&cleaned) {
        return result;
    }
    if let Some(result) = test_runner::try_compress(&cleaned) {
        return result;
    }
    if let Some(result) = npm::try_compress(&cleaned) {
        return result;
    }
    if let Some(result) = grep::try_compress(&cleaned) {
        return result;
    }

    debug!(
        output_len = cleaned.len(),
        "output compress: no pattern matched, returning cleaned text"
    );
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("no escapes"), "no escapes");
    }

    #[test]
    fn test_collapse_blank_lines() {
        assert_eq!(collapse_blank_lines("a\n\n\n\nb"), "a\n\nb");
        assert_eq!(collapse_blank_lines("a\n\nb"), "a\n\nb");
    }

    #[test]
    fn test_strip_progress_bars() {
        assert_eq!(
            strip_progress_bars("downloading\r50%\r100%\ndone"),
            "\ndone"
        );
        assert_eq!(strip_progress_bars("normal line\n"), "normal line\n");
    }
}
