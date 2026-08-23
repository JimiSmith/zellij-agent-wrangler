//! What tmux says about the windows and the panes of one session, as the
//! vocabulary that the shared sidebar takes.
//!
//! Nothing here runs a program. Every function takes the text that tmux wrote
//! and gives back values, so a test needs no tmux server and no pty.
//!
//! # Two questions and not one
//!
//! A window name and a pane title are both free text, and either can hold a
//! tab. One query carrying both would need a repair when a split came out with
//! the wrong number of fields. Two queries each put their one free text field
//! last, so a split into a fixed number of parts is exact and cannot fail.
//!
//! # The order and the number are two facts
//!
//! `TabPosition` is the order of the window in the list. It is contiguous and
//! it starts at zero, because the shared code joins a tab report to a session
//! layout on it.
//!
//! `displayed_index` is the number that tmux gives the window. A user sets
//! `base-index` to any value, and a closed window leaves a gap. The user types
//! that number to reach the window, so the row draws that number.

use agent_wrangler_sidebar::{
    Focus, FocusTarget, PaneId, PaneReport, PaneVisibility, SessionLayout, SidebarPaneReport,
    TabId, TabLayout, TabReport,
};
use agent_wrangler_ui::model::TabPosition;

/// The character that separates the fields of one reported line.
///
/// The format strings below put this between the fields. Tmux copies a
/// character it does not know into its output as it stands, so this reaches the
/// output as one tab.
const FIELD_BREAK: char = '\t';

/// What tmux writes for a flag that is on.
const TMUX_TRUE: &str = "1";

/// The format that asks tmux to describe every window of a session.
///
/// The name comes last, because a window name is free text and can hold a tab.
/// Every field in front of it is a fixed shape, so a split into four parts
/// gives the name whole however many tabs are in it.
///
/// These words are a contract with another program. A mistake in them compiles,
/// and it fails only against a real tmux.
pub const WINDOW_FORMAT: &str = "#{window_id}\t#{window_index}\t#{window_active}\t#{window_name}";

/// The format that asks tmux to describe every pane of a session.
///
/// The title comes last, for the same reason that the window name does.
///
/// The title is not `#{pane_title}` alone. `pane_title` holds the title that a
/// program set with an escape sequence. A shell prompt sets one. A program such
/// as `sleep` sets none, and tmux then answers the host name for every such
/// pane. A column of host names says nothing about the panes.
///
/// So the last field asks whether the title is still the host name, and names
/// the running program when it is. That matches what a zellij sidebar draws,
/// because zellij reports the same escape sequence title and falls back to the
/// command in the same way.
pub const PANE_FORMAT: &str = "#{window_id}\t#{pane_id}\t#{pane_active}\t\
     #{?#{==:#{pane_title},#{host_short}},#{pane_current_command},#{pane_title}}";

/// How many fields each format writes.
const WINDOW_FIELDS: usize = 4;
const PANE_FIELDS: usize = 4;

/// One window, as tmux reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportedWindow {
    /// The window's own stable identity, such as `@3`.
    pub id: String,
    /// The number that tmux calls the window, and that the user types.
    pub index: String,
    /// Whether this is the window that the session shows.
    pub active: bool,
    pub name: String,
}

/// One pane, as tmux reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportedPane {
    /// The window that holds this pane.
    pub window_id: String,
    /// The pane's own stable identity, such as `%12`.
    pub id: String,
    /// Whether this is the pane that its window shows as active. Tmux marks one
    /// pane per window, so this is true in a window that the user is not in.
    pub active: bool,
    pub title: String,
}

/// The windows that one `list-windows` answer describes.
///
/// A line with the wrong number of fields is dropped. Tmux writes one line per
/// window with the format that it was given, so such a line comes from a tmux
/// that answered a different question.
pub fn read_windows(answer: &str) -> Vec<ReportedWindow> {
    answer
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.splitn(WINDOW_FIELDS, FIELD_BREAK).collect();
            let [id, index, active, name] = fields.as_slice() else {
                return None;
            };
            Some(ReportedWindow {
                id: (*id).to_string(),
                index: (*index).to_string(),
                active: *active == TMUX_TRUE,
                name: (*name).to_string(),
            })
        })
        .collect()
}

/// The panes that one `list-panes` answer describes.
pub fn read_panes(answer: &str) -> Vec<ReportedPane> {
    answer
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.splitn(PANE_FIELDS, FIELD_BREAK).collect();
            let [window_id, id, active, title] = fields.as_slice() else {
                return None;
            };
            Some(ReportedPane {
                window_id: (*window_id).to_string(),
                id: (*id).to_string(),
                active: *active == TMUX_TRUE,
                title: (*title).to_string(),
            })
        })
        .collect()
}

/// The tab reports that the shared sidebar takes.
///
/// The position is the place in this list and never the tmux index. The tmux
/// index goes to `displayed_index`, and the row draws `displayed_index`.
pub fn tab_reports(windows: &[ReportedWindow]) -> Vec<TabReport> {
    windows
        .iter()
        .enumerate()
        .map(|(order, window)| TabReport {
            id: TabId::new(window.id.clone()),
            position: TabPosition::at(order),
            displayed_index: window.index.clone(),
            name: window.name.clone(),
            active: window.active,
        })
        .collect()
}

/// The session layout that the shared sidebar takes.
///
/// `sidebar_pane` names the pane that this program runs in, and this function
/// leaves that pane out of the content panes. A sidebar that drew itself as a
/// pane would give the user a row for the pane they already look at.
///
/// Two fields are fixed here because tmux has nothing that answers for them.
/// Tmux parks no pane, so every pane is on screen. Tmux runs no plugin, so no
/// other kind of pane can hold the focus.
pub fn session_layout(
    windows: &[ReportedWindow],
    panes: &[ReportedPane],
    own_pane: &str,
) -> SessionLayout {
    let tabs = windows
        .iter()
        .enumerate()
        .map(|(order, window)| {
            let mine: Vec<&ReportedPane> = panes
                .iter()
                .filter(|pane| pane.window_id == window.id)
                .collect();
            TabLayout {
                position: TabPosition::at(order),
                other_focused: false,
                sidebar_pane: mine.iter().find(|pane| pane.id == own_pane).map(|pane| {
                    SidebarPaneReport {
                        focused: window.active && pane.active,
                    }
                }),
                content_panes: mine
                    .iter()
                    .filter(|pane| pane.id != own_pane)
                    .map(|pane| PaneReport {
                        id: PaneId::new(pane.id.clone()),
                        title: pane.title.clone(),
                        focused: window.active && pane.active,
                        visibility: PaneVisibility::OnScreen,
                    })
                    .collect(),
            }
        })
        .collect();
    SessionLayout { tabs }
}

/// Where the user is, or `None` when the reports name no active window.
///
/// Tmux marks one active pane in every window. The user sits in the active
/// pane of the active window alone, so this function answers only when both
/// flags are set.
pub fn focus(windows: &[ReportedWindow], panes: &[ReportedPane], own_pane: &str) -> Option<Focus> {
    let window = windows.iter().find(|window| window.active)?;
    let pane = panes
        .iter()
        .find(|pane| pane.window_id == window.id && pane.active)?;
    let target = if pane.id == own_pane {
        FocusTarget::Sidebar
    } else {
        FocusTarget::Content(PaneId::new(pane.id.clone()))
    };
    Some(Focus {
        tab: TabId::new(window.id.clone()),
        target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session of two windows, whose indexes have a gap where window 2
    /// closed. The user is in the second pane of window 1.
    const WINDOWS: &str = "@1\t1\t1\teditor\n@7\t4\t0\tbuild logs\n";
    const PANES: &str =
        "@1\t%0\t0\tnvim\n@1\t%3\t1\tbash\n@1\t%9\t0\tsidebar\n@7\t%5\t1\ttail -f\n";

    fn windows() -> Vec<ReportedWindow> {
        read_windows(WINDOWS)
    }

    fn panes() -> Vec<ReportedPane> {
        read_panes(PANES)
    }

    #[test]
    fn a_window_line_splits_into_its_four_fields() {
        assert_eq!(
            windows(),
            vec![
                ReportedWindow {
                    id: "@1".to_string(),
                    index: "1".to_string(),
                    active: true,
                    name: "editor".to_string(),
                },
                ReportedWindow {
                    id: "@7".to_string(),
                    index: "4".to_string(),
                    active: false,
                    name: "build logs".to_string(),
                },
            ]
        );
    }

    #[test]
    fn a_name_that_holds_a_tab_survives_whole() {
        // The name is the last field, so the split stops counting before the
        // name and gives it back whole. A tab inside the name therefore changes
        // no other field.
        let read = read_windows("@1\t1\t1\tone\ttwo\tthree\n");
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].name, "one\ttwo\tthree");
        assert_eq!(read[0].index, "1");
    }

    #[test]
    fn a_title_that_holds_a_tab_survives_whole() {
        let read = read_panes("@1\t%0\t1\tvim\tnotes.txt\n");
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].title, "vim\tnotes.txt");
        assert_eq!(read[0].id, "%0");
    }

    #[test]
    fn a_line_of_the_wrong_shape_is_dropped() {
        // Tmux writes the format that it was given. A line of another shape
        // came from a tmux that answered a different question, and reading it
        // would put a wrong value under a right field name.
        assert!(read_windows("@1\t1\n").is_empty());
        assert!(read_panes("nothing here\n").is_empty());
        assert!(read_windows("").is_empty());
    }

    #[test]
    fn an_empty_name_is_still_a_window() {
        let read = read_windows("@1\t1\t1\t\n");
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].name, "");
    }

    #[test]
    fn the_row_draws_the_index_that_tmux_gives_and_not_the_order() {
        let reports = tab_reports(&windows());
        assert_eq!(
            reports
                .iter()
                .map(|tab| tab.displayed_index.as_str())
                .collect::<Vec<_>>(),
            ["1", "4"]
        );
        // The position is the order, so the shared code can join a report to a
        // layout on it. A gap in the tmux index leaves no gap here.
        assert_eq!(
            reports
                .iter()
                .map(|tab| tab.position.zero_based())
                .collect::<Vec<_>>(),
            [0, 1]
        );
    }

    #[test]
    fn a_window_is_identified_by_its_own_id_and_never_by_its_index() {
        let reports = tab_reports(&windows());
        assert_eq!(reports[0].id, TabId::new("@1"));
        assert_eq!(reports[1].id, TabId::new("@7"));
    }

    #[test]
    fn the_pane_of_this_program_is_the_sidebar_pane_and_not_a_content_pane() {
        let layout = session_layout(&windows(), &panes(), "%9");
        let first = &layout.tabs[0];
        assert_eq!(
            first
                .content_panes
                .iter()
                .map(|pane| pane.id.as_str())
                .collect::<Vec<_>>(),
            ["%0", "%3"]
        );
        assert_eq!(
            first.sidebar_pane,
            Some(SidebarPaneReport { focused: false })
        );
        // The sidebar runs in one window of the session. Every other window
        // reports no sidebar pane.
        assert_eq!(layout.tabs[1].sidebar_pane, None);
    }

    #[test]
    fn only_the_active_pane_of_the_active_window_is_focused() {
        let layout = session_layout(&windows(), &panes(), "%9");
        let focused: Vec<&str> = layout
            .tabs
            .iter()
            .flat_map(|tab| tab.content_panes.iter())
            .filter(|pane| pane.focused)
            .map(|pane| pane.id.as_str())
            .collect();
        // `%5` is the active pane of window `@7`, and the user is not in that
        // window. Tmux marks an active pane in every window, so the window
        // flag settles the answer.
        assert_eq!(focused, ["%3"]);
    }

    #[test]
    fn every_tmux_pane_is_on_screen_and_no_other_pane_holds_the_focus() {
        let layout = session_layout(&windows(), &panes(), "%9");
        for tab in &layout.tabs {
            assert!(!tab.other_focused);
            for pane in &tab.content_panes {
                assert_eq!(pane.visibility, PaneVisibility::OnScreen);
            }
        }
    }

    #[test]
    fn the_focus_names_the_active_pane_of_the_active_window() {
        assert_eq!(
            focus(&windows(), &panes(), "%9"),
            Some(Focus {
                tab: TabId::new("@1"),
                target: FocusTarget::Content(PaneId::new("%3")),
            })
        );
    }

    #[test]
    fn the_focus_says_sidebar_when_the_user_sits_in_this_program() {
        // The same reports, read by the program in pane `%3`. That pane is the
        // active one, so this program is the one holding the focus.
        assert_eq!(
            focus(&windows(), &panes(), "%3"),
            Some(Focus {
                tab: TabId::new("@1"),
                target: FocusTarget::Sidebar,
            })
        );
    }

    #[test]
    fn reports_with_no_active_window_place_nobody() {
        let quiet = read_windows("@1\t1\t0\teditor\n");
        assert_eq!(focus(&quiet, &panes(), "%9"), None);
    }

    #[test]
    fn an_active_window_with_no_active_pane_places_nobody() {
        let headless = read_panes("@1\t%0\t0\tnvim\n");
        assert_eq!(focus(&windows(), &headless, "%9"), None);
    }

    #[test]
    fn a_window_that_lists_no_pane_still_draws_a_row() {
        let layout = session_layout(&windows(), &read_panes("@1\t%0\t1\tnvim\n"), "%9");
        assert_eq!(layout.tabs.len(), 2);
        assert!(layout.tabs[1].content_panes.is_empty());
    }

    #[test]
    fn the_formats_ask_for_the_fields_in_the_order_that_the_readers_expect() {
        // These strings are a contract with tmux. A field moved here compiles,
        // and it fails only against a real server.
        assert_eq!(
            WINDOW_FORMAT.split(FIELD_BREAK).collect::<Vec<_>>(),
            [
                "#{window_id}",
                "#{window_index}",
                "#{window_active}",
                "#{window_name}"
            ]
        );
        assert_eq!(
            PANE_FORMAT.split(FIELD_BREAK).collect::<Vec<_>>(),
            [
                "#{window_id}",
                "#{pane_id}",
                "#{pane_active}",
                // The title falls back to the running program, because tmux
                // answers the host name for a pane that set no title.
                "#{?#{==:#{pane_title},#{host_short}},#{pane_current_command},#{pane_title}}"
            ]
        );
        // The free text field is last in both, so the split is exact.
        assert_eq!(WINDOW_FORMAT.split(FIELD_BREAK).count(), WINDOW_FIELDS);
        assert_eq!(PANE_FORMAT.split(FIELD_BREAK).count(), PANE_FIELDS);
    }
}
