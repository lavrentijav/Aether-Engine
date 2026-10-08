//! Paged chat output.
//!
//! Chat is a bad terminal: ten lines and the rest has scrolled away. Every
//! command that can return more than a screenful renders its whole answer once,
//! parks it here, and sends one page — so `/page 3` costs nothing and, more
//! importantly, page 3 still shows the *same* thing it would have shown when
//! the query ran, rather than re-querying a world that has moved on.

/// Lines per page. Ten fits the default chat window with the header and footer
/// still visible.
pub const PAGE: usize = 10;

/// One rendered answer, held for as long as the player might page through it.
#[derive(Default)]
pub struct Pager {
    title: String,
    lines: Vec<String>,
    page: usize,
}

impl Pager {
    /// Park a rendered answer, showing page 1.
    pub fn set(&mut self, title: impl Into<String>, lines: Vec<String>) {
        self.title = title.into();
        self.lines = lines;
        self.page = 0;
    }

    /// Whether anything has been parked yet.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Total pages, at least one so "page 1 of 0" never appears.
    pub fn pages(&self) -> usize {
        self.lines.len().div_ceil(PAGE).max(1)
    }

    /// Move `delta` pages, clamped to the ends. Returns the rendered page.
    pub fn step(&mut self, delta: isize) -> Vec<String> {
        let last = self.pages() - 1;
        self.page = (self.page as isize + delta).clamp(0, last as isize) as usize;
        self.render()
    }

    /// Jump to a 1-based page number, clamped. Returns the rendered page.
    pub fn goto(&mut self, page: usize) -> Vec<String> {
        self.page = page.saturating_sub(1).min(self.pages() - 1);
        self.render()
    }

    /// The current page, with a header and — only when there is more than one
    /// page — a footer saying how to reach the rest.
    pub fn render(&self) -> Vec<String> {
        if self.lines.is_empty() {
            return vec![format!("{} — nothing found", self.title)];
        }
        let start = self.page * PAGE;
        let end = (start + PAGE).min(self.lines.len());
        let mut out = Vec::with_capacity(PAGE + 2);
        out.push(format!(
            "{} — page {} of {} ({} entries)",
            self.title,
            self.page + 1,
            self.pages(),
            self.lines.len()
        ));
        out.extend_from_slice(&self.lines[start..end]);
        if self.pages() > 1 {
            let mut nav = Vec::new();
            if self.page > 0 {
                nav.push("/page prev".to_string());
            }
            if self.page + 1 < self.pages() {
                nav.push("/page next".to_string());
            }
            out.push(format!("  {}", nav.join("  |  ")));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line {i}")).collect()
    }

    #[test]
    fn a_short_answer_is_one_page_with_no_navigation() {
        let mut p = Pager::default();
        p.set("t", lines(3));
        let out = p.render();
        assert_eq!(out.len(), 1 + 3, "header plus the lines, no footer");
        assert!(out[0].contains("page 1 of 1"));
    }

    #[test]
    fn page_boundaries_are_exact() {
        let mut p = Pager::default();
        p.set("t", lines(PAGE));
        assert_eq!(p.pages(), 1, "exactly one full page is one page");
        p.set("t", lines(PAGE + 1));
        assert_eq!(p.pages(), 2, "one line over is two");
    }

    #[test]
    fn paging_past_either_end_clamps_rather_than_wrapping() {
        // Wrapping would be worse than useless: an operator holding `next`
        // would silently start re-reading page one and think the log repeated.
        let mut p = Pager::default();
        p.set("t", lines(25));
        p.step(-5);
        assert!(p.render()[0].contains("page 1 of 3"));
        p.step(99);
        assert!(p.render()[0].contains("page 3 of 3"));
    }

    #[test]
    fn the_last_page_is_short_not_padded() {
        let mut p = Pager::default();
        p.set("t", lines(PAGE + 2));
        let out = p.goto(2);
        // header + 2 lines + nav
        assert_eq!(out.len(), 4);
        assert_eq!(out[1], "line 10");
    }

    #[test]
    fn navigation_only_offers_directions_that_exist() {
        let mut p = Pager::default();
        p.set("t", lines(25));
        assert!(!p.render().last().unwrap().contains("prev"));
        p.goto(2);
        let mid = p.render();
        assert!(mid.last().unwrap().contains("prev") && mid.last().unwrap().contains("next"));
        p.goto(3);
        assert!(!p.render().last().unwrap().contains("next"));
    }

    #[test]
    fn an_empty_answer_says_so_instead_of_showing_an_empty_page() {
        let mut p = Pager::default();
        p.set("changes at 0 0 0", vec![]);
        assert_eq!(p.render(), vec!["changes at 0 0 0 — nothing found"]);
    }
}
