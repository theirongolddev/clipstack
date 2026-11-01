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

/// Age as exactly 4 columns, right-aligned: `  5s`, ` 12m`, `  3h`,
/// ` 40d`, capped at `999d`; a future time (clock skew) is `  0s`.
pub fn format_compact_age(ts_ms: i64, now_ms: i64) -> String {
    let secs = now_ms.saturating_sub(ts_ms).max(0) / 1000;
    let (n, unit) = match secs {
        0..=59 => (secs, 's'),
        60..=3599 => (secs / 60, 'm'),
        3600..=86_399 => (secs / 3600, 'h'),
        _ => ((secs / 86_400).min(999), 'd'),
    };
    format!("{n:>3}{unit}")
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

    const SEC: i64 = 1000;
    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn compact_age_units_and_width_4() {
        let cases = [
            (0, "  0s"),
            (5 * SEC, "  5s"),
            (59 * SEC, " 59s"),
            (60 * SEC, "  1m"),
            (12 * 60 * SEC, " 12m"),
            (3 * 3600 * SEC, "  3h"),
            (23 * 3600 * SEC, " 23h"),
            (40 * 86400 * SEC, " 40d"),
            (365 * 86400 * SEC, "365d"),
        ];
        for (age, want) in cases {
            let got = format_compact_age(NOW - age, NOW);
            assert_eq!(got, want, "age {age} ms");
            assert_eq!(got.chars().count(), 4);
        }
    }

    #[test]
    fn caps_at_999d() {
        assert_eq!(format_compact_age(NOW - 999 * 86400 * SEC, NOW), "999d");
        assert_eq!(format_compact_age(NOW - 5000 * 86400 * SEC, NOW), "999d");
        assert_eq!(format_compact_age(1, NOW), "999d", "an import dated 1970");
        assert_eq!(format_compact_age(i64::MIN, NOW), "999d", "no overflow");
    }

    #[test]
    fn future_clamps_to_0s() {
        assert_eq!(format_compact_age(NOW + 3600 * SEC, NOW), "  0s");
        assert_eq!(format_compact_age(i64::MAX, NOW), "  0s");
    }

    #[test]
    fn cjk_counts_double_width() {
        assert_eq!(wrap_to_width(&"中".repeat(5), 4), ["中中", "中中", "中"]);
        assert_eq!(wrap_to_width("a中中", 4), ["a中", "中"], "a wide char never straddles the edge");
        assert_eq!(wrap_to_width("中", 1), ["中"], "a char wider than the pane gets its own line");
        assert_eq!(wrap_to_width("e\u{301}x", 2), ["e\u{301}x"], "combining marks take no column");
    }
}
