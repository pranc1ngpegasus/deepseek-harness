//! Bounded tool output so one command cannot fill the context window.

use alloc::string::{String, ToString};

/// Keep at most `max_chars` characters, naming how many were dropped.
///
/// The head and tail are both kept so a failing command's first error and its
/// final summary survive. Counting is by Unicode scalar value, never by byte.
#[must_use]
pub fn truncate_output(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let head = max_chars / 2;
    let tail = max_chars - head;
    let omitted = count - max_chars;
    let mut output = String::with_capacity(text.len());
    output.extend(text.chars().take(head));
    let _ = core::fmt::Write::write_fmt(
        &mut output,
        format_args!("\n...[truncated {omitted} chars]...\n"),
    );
    output.extend(text.chars().skip(count - tail));
    output
}

#[cfg(test)]
mod tests {
    use super::truncate_output;

    #[test]
    fn short_output_is_unchanged() {
        assert_eq!(truncate_output("hello", 10), "hello");
        assert_eq!(truncate_output("hello", 5), "hello");
        assert_eq!(truncate_output("", 5), "");
    }

    #[test]
    fn long_output_keeps_both_ends_and_names_the_loss() {
        let text = "0123456789".repeat(10);
        let truncated = truncate_output(&text, 20);
        assert!(truncated.starts_with("0123456789"));
        assert!(truncated.ends_with("0123456789"));
        assert!(truncated.contains("truncated 80 chars"));
    }

    #[test]
    fn counting_uses_characters_not_bytes() {
        let text = "あいうえおかきくけこ";
        assert_eq!(
            truncate_output(text, 6),
            "あいう\n...[truncated 4 chars]...\nくけこ"
        );
    }

    #[test]
    fn a_zero_budget_reports_the_omission_only() {
        let truncated = truncate_output("abc", 0);
        assert_eq!(truncated, "\n...[truncated 3 chars]...\n");
    }
}
