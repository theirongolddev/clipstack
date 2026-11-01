//! Text layout for the preview pane.

/// Wrap lines to preview width (typically terminal width - padding)
pub const WRAP_WIDTH: usize = 80;

/// Split `content` into lines of at most `width` characters.
pub fn wrap_chars(content: &str, width: usize) -> Vec<String> {
    content
        .lines()
        .flat_map(|line| {
            // Use char count, not byte length, for correct Unicode handling
            if line.chars().count() <= width {
                vec![line.to_string()]
            } else {
                line.chars()
                    .collect::<Vec<_>>()
                    .chunks(width)
                    .map(|c| c.iter().collect::<String>())
                    .collect()
            }
        })
        .collect()
}
