//! Typed page chrome for logged-in pages.
//!
//! Every template that extends `app.html` carries a [`Chrome`] field; the
//! shared shell renders the top bar and bottom tab bar from it, so a page
//! that forgets its chrome fails to compile.

/// One bottom-bar tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Current,
    Standings,
    Contests,
    MyTeam,
}

/// Top-bar and tab-bar state for one page.
pub struct Chrome {
    /// Bar text and the `<title>` tag.
    pub title: String,
    /// `Some(href)` renders a focused flow: ✕ to `href`, centered title,
    /// no tab bar. `None` renders a left-aligned title and the tab bar,
    /// whose More item carries the overflow menu.
    pub close: Option<String>,
    /// The lit tab. `None` with `close: None` shows the bar with nothing
    /// lit (admin, invites).
    pub tab: Option<Tab>,
}

impl Chrome {
    /// A browsing page: tab bar with `tab` lit.
    pub fn tabbed(title: impl Into<String>, tab: Tab) -> Self {
        Self {
            title: title.into(),
            close: None,
            tab: Some(tab),
        }
    }
    /// A contest page: the Current tab when it is the request's current
    /// contest, otherwise the Contests tab.
    pub(crate) fn contest(title: impl Into<String>, contest_id: i64) -> Self {
        let tab = if crate::context::current_contest() == Some(contest_id) {
            Tab::Current
        } else {
            Tab::Contests
        };
        Self::tabbed(title, tab)
    }

    /// A browsing page outside the four tabs: tab bar, nothing lit.
    pub fn unlit(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            close: None,
            tab: None,
        }
    }

    /// A focused flow: ✕ to `close`, centered title, no tab bar.
    pub fn focused(title: impl Into<String>, close: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            close: Some(close.into()),
            tab: None,
        }
    }

    // Lit-tab tests, one per tab, callable from Askama.
    pub fn is_current(&self) -> bool {
        self.tab == Some(Tab::Current)
    }
    pub fn is_standings(&self) -> bool {
        self.tab == Some(Tab::Standings)
    }
    pub fn is_contests(&self) -> bool {
        self.tab == Some(Tab::Contests)
    }
    pub fn is_my_team(&self) -> bool {
        self.tab == Some(Tab::MyTeam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabbed_lights_its_tab_and_shows_no_close() {
        let c = Chrome::tabbed("Contests", Tab::Contests);
        assert_eq!(c.title, "Contests");
        assert!(c.close.is_none());
        assert!(c.is_contests());
        assert!(!c.is_current());
    }

    #[test]
    fn unlit_shows_the_bar_with_nothing_lit() {
        let c = Chrome::unlit("Admin Tools");
        assert!(c.close.is_none());
        assert!(c.tab.is_none());
    }

    #[test]
    fn focused_carries_its_close_target() {
        let c = Chrome::focused("Edit Lineup", "/contests/7");
        assert_eq!(c.close.as_deref(), Some("/contests/7"));
        assert!(c.tab.is_none());
    }
}
