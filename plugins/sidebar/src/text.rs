//! Small text helpers for the sidebar's own configured strings.

/// Translate `&`-style colour codes (`&a`, `&l`) into the `§` codes the
/// client's text renderer expects. A lone `&` is left alone.
pub fn colorize(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            match chars.peek() {
                Some(next) if is_color_code(*next) => {
                    out.push('\u{00a7}');
                    out.push(*next);
                    chars.next();
                }
                _ => out.push('&'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Whether `c` opens a legacy formatting code.
fn is_color_code(c: char) -> bool {
    matches!(
        c.to_ascii_lowercase(),
        '0'..='9' | 'a'..='f' | 'k' | 'l' | 'm' | 'n' | 'o' | 'r'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ampersand_codes_become_section_codes() {
        assert_eq!(colorize("&6Hello &lWorld"), "§6Hello §lWorld");
        assert_eq!(colorize("a&Qb"), "a&Qb", "a lone & is literal");
        assert_eq!(colorize("100&"), "100&");
    }
}
