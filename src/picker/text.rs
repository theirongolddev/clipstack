//! Text layout for the preview pane.

use unicode_width::UnicodeWidthChar;

/// Columns between tab stops in the preview
pub const TAB_WIDTH: usize = 4;

/// Split `content` into rows of at most `width` display columns (CJK counts
/// 2, combining marks 0). Tabs expand to the next stop, `\r\n` ends a line
/// like `\n`, and a character wider than the pane gets a row of its own.
pub fn wrap_to_width(content: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in content.lines() {
        let mut row = String::new();
        let mut used = 0;
        // Columns from the start of the logical line, for tab stops
        let mut column = 0;
        for c in line.chars() {
            let w = match c {
                '\t' => TAB_WIDTH - column % TAB_WIDTH,
                _ => c.width().unwrap_or(0),
            };
            if used + w > width && used > 0 {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            match c {
                '\t' => row.extend(std::iter::repeat_n(' ', w)),
                _ => row.push(c),
            }
            used += w;
            column += w;
        }
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_given_width() {
        assert_eq!(wrap_to_width("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap_to_width("abcd", 4), ["abcd"], "an exact fit is one line");
        assert_eq!(wrap_to_width("a\n\nb", 4), ["a", "", "b"], "blank lines are kept");
        assert_eq!(wrap_to_width("x\r\ny\n", 4), ["x", "y"], "CRLF and a final newline");
        assert_eq!(wrap_to_width("a\tb", 10), ["a   b"], "tabs expand to the next stop of 4");
        assert_eq!(wrap_to_width("\tabcdef", 6), ["    ab", "cdef"]);
        assert!(wrap_to_width("", 4).is_empty());
        assert_eq!(wrap_to_width("abc", 0), ["a", "b", "c"], "a zero width still makes progress");
    }

    #[test]
    fn cjk_counts_double_width() {
        assert_eq!(wrap_to_width(&"中".repeat(5), 4), ["中中", "中中", "中"]);
        assert_eq!(wrap_to_width("a中中", 4), ["a中", "中"], "a wide char never straddles the edge");
        assert_eq!(wrap_to_width("中", 1), ["中"], "a char wider than the pane gets its own line");
        assert_eq!(wrap_to_width("e\u{301}x", 2), ["e\u{301}x"], "combining marks take no column");
    }
}
