//! The in-app editor for application names and categories (the `a` key).
//!
//! Flow: pick an application, confirm or change its short name, pick a category (or type a
//! new one). The result is appended to `apps.conf` by the caller; this file only holds the
//! state machine and the key handling, so it can be tested without a terminal or any files.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use timesink_core::appmap::{AppTotal, OTHER};

/// Longest name or category the editor will accept.
const MAX_TEXT: usize = 40;

/// Where the editor is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Assign {
    /// Not open.
    #[default]
    Closed,
    /// Choosing which application to edit. `list` is frozen when the editor opens so the
    /// 5-second refresh cannot shuffle rows under the cursor.
    PickApp {
        /// The applications to choose from, in the order they were on screen.
        list: Vec<AppTotal>,
        /// Highlighted row.
        sel: usize,
    },
    /// Typing the short name (starts as the current one).
    Name {
        /// The application being edited.
        target: AppTotal,
        /// The name typed so far.
        text: String,
    },
    /// Choosing a category from the known ones, "Other", or a new one.
    PickCategory {
        /// The application being edited.
        target: AppTotal,
        /// The name chosen in the previous step.
        name: String,
        /// Highlighted row.
        sel: usize,
    },
    /// Typing a brand-new category name.
    NewCategory {
        /// The application being edited.
        target: AppTotal,
        /// The name chosen earlier.
        name: String,
        /// The category typed so far.
        text: String,
    },
}

/// What the caller must write once the editor finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    /// Raw classes to give the entry to (one `apps.conf` line each).
    pub raws: Vec<String>,
    /// The short name.
    pub display: String,
    /// The category.
    pub category: String,
}

/// The rows of the category list: the configured categories, then "Other", then the
/// "new category" action.
pub fn category_rows(known: &[String]) -> Vec<String> {
    let mut rows: Vec<String> = known.iter().filter(|c| *c != OTHER).cloned().collect();
    rows.push(OTHER.to_string());
    rows.push("+ new category…".to_string());
    rows
}

/// Open the editor on a frozen copy of the application rows. Does nothing if there are none.
pub fn open(apps: Vec<AppTotal>) -> Assign {
    if apps.is_empty() {
        Assign::Closed
    } else {
        Assign::PickApp { list: apps, sel: 0 }
    }
}

/// Move a highlight up or down within `len` rows, wrapping round.
fn step(sel: usize, len: usize, down: bool) -> usize {
    if len == 0 {
        0
    } else if down {
        (sel + 1) % len
    } else {
        (sel + len - 1) % len
    }
}

/// Apply typing to a text field: printable characters append, Backspace deletes.
fn edit_text(text: &mut String, key: &KeyEvent) {
    match key.code {
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && text.chars().count() < MAX_TEXT =>
        {
            text.push(c)
        }
        KeyCode::Backspace => {
            text.pop();
        }
        _ => {}
    }
}

/// Handle one key press while the editor is open. Returns `Some(Write)` when the user has
/// finished; the editor state is updated (or closed) in place.
pub fn handle_key(state: &mut Assign, key: KeyEvent, known: &[String]) -> Option<Write> {
    // Esc always abandons the whole edit.
    if key.code == KeyCode::Esc {
        *state = Assign::Closed;
        return None;
    }
    let current = std::mem::take(state);
    let (next, done) = match current {
        Assign::Closed => (Assign::Closed, None),
        Assign::PickApp { list, mut sel } => match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                sel = step(sel, list.len(), true);
                (Assign::PickApp { list, sel }, None)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                sel = step(sel, list.len(), false);
                (Assign::PickApp { list, sel }, None)
            }
            KeyCode::Enter => {
                let target = list[sel.min(list.len() - 1)].clone();
                let text = target.display.clone();
                (Assign::Name { target, text }, None)
            }
            _ => (Assign::PickApp { list, sel }, None),
        },
        Assign::Name { target, mut text } => match key.code {
            KeyCode::Enter if !text.trim().is_empty() => {
                // Start the category list on the app's current category.
                let rows = category_rows(known);
                let sel = rows.iter().position(|r| *r == target.category).unwrap_or(0);
                (
                    Assign::PickCategory {
                        target,
                        name: text.trim().to_string(),
                        sel,
                    },
                    None,
                )
            }
            _ => {
                edit_text(&mut text, &key);
                (Assign::Name { target, text }, None)
            }
        },
        Assign::PickCategory {
            target,
            name,
            mut sel,
        } => {
            let rows = category_rows(known);
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    sel = step(sel, rows.len(), true);
                    (Assign::PickCategory { target, name, sel }, None)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    sel = step(sel, rows.len(), false);
                    (Assign::PickCategory { target, name, sel }, None)
                }
                KeyCode::Enter if sel + 1 == rows.len() => (
                    Assign::NewCategory {
                        target,
                        name,
                        text: String::new(),
                    },
                    None,
                ),
                KeyCode::Enter => {
                    let write = Write {
                        raws: target.raws.clone(),
                        display: name,
                        category: rows[sel].clone(),
                    };
                    (Assign::Closed, Some(write))
                }
                _ => (Assign::PickCategory { target, name, sel }, None),
            }
        }
        Assign::NewCategory {
            target,
            name,
            mut text,
        } => match key.code {
            KeyCode::Enter if !text.trim().is_empty() => {
                let write = Write {
                    raws: target.raws.clone(),
                    display: name,
                    category: text.trim().to_string(),
                };
                (Assign::Closed, Some(write))
            }
            _ => {
                edit_text(&mut text, &key);
                (Assign::NewCategory { target, name, text }, None)
            }
        },
    };
    *state = next;
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key press with no modifiers.
    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Type a string one character at a time.
    fn type_str(state: &mut Assign, text: &str, known: &[String]) {
        for c in text.chars() {
            handle_key(state, k(KeyCode::Char(c)), known);
        }
    }

    fn satty() -> AppTotal {
        AppTotal {
            display: "com.gabm.satty".into(),
            category: "Other".into(),
            secs: 60,
            raws: vec!["com.gabm.satty".into()],
        }
    }

    fn known() -> Vec<String> {
        vec!["Terminal".into(), "Utilities".into()]
    }

    #[test]
    fn rename_and_pick_an_existing_category() {
        let known = known();
        let mut st = open(vec![satty()]);
        assert!(matches!(st, Assign::PickApp { .. }));
        handle_key(&mut st, k(KeyCode::Enter), &known);
        // Replace the name: delete the old one, type the new one.
        for _ in 0..14 {
            handle_key(&mut st, k(KeyCode::Backspace), &known);
        }
        type_str(&mut st, "satty", &known);
        handle_key(&mut st, k(KeyCode::Enter), &known);
        // Rows are Terminal, Utilities, Other, + new; the list starts on the current
        // category (Other), so move up once to Utilities.
        handle_key(&mut st, k(KeyCode::Up), &known);
        let done = handle_key(&mut st, k(KeyCode::Enter), &known).expect("finished");
        assert_eq!(
            done,
            Write {
                raws: vec!["com.gabm.satty".into()],
                display: "satty".into(),
                category: "Utilities".into()
            }
        );
        assert_eq!(st, Assign::Closed);
    }

    #[test]
    fn a_new_category_can_be_typed() {
        let known = known();
        let mut st = open(vec![satty()]);
        handle_key(&mut st, k(KeyCode::Enter), &known); // pick app
        handle_key(&mut st, k(KeyCode::Enter), &known); // keep the name
        handle_key(&mut st, k(KeyCode::Down), &known); // Other -> "+ new"
        handle_key(&mut st, k(KeyCode::Enter), &known);
        assert!(matches!(st, Assign::NewCategory { .. }));
        // An empty name is not accepted.
        assert!(handle_key(&mut st, k(KeyCode::Enter), &known).is_none());
        type_str(&mut st, "Screenshots", &known);
        let done = handle_key(&mut st, k(KeyCode::Enter), &known).unwrap();
        assert_eq!(done.category, "Screenshots");
        assert_eq!(done.display, "com.gabm.satty");
    }

    #[test]
    fn escape_cancels_at_any_step_and_q_is_just_a_letter_in_text() {
        let known = known();
        let mut st = open(vec![satty()]);
        handle_key(&mut st, k(KeyCode::Enter), &known);
        type_str(&mut st, "q", &known);
        assert!(matches!(&st, Assign::Name { text, .. } if text.ends_with('q')));
        assert!(handle_key(&mut st, k(KeyCode::Esc), &known).is_none());
        assert_eq!(st, Assign::Closed);
    }

    #[test]
    fn nothing_to_edit_stays_closed_and_selection_wraps() {
        assert_eq!(open(vec![]), Assign::Closed);
        assert_eq!(step(0, 3, false), 2);
        assert_eq!(step(2, 3, true), 0);
        assert_eq!(step(0, 0, true), 0);
    }

    #[test]
    fn text_is_capped() {
        let known = known();
        let mut st = open(vec![satty()]);
        handle_key(&mut st, k(KeyCode::Enter), &known);
        type_str(&mut st, &"x".repeat(100), &known);
        match &st {
            Assign::Name { text, .. } => assert_eq!(text.chars().count(), MAX_TEXT),
            other => panic!("unexpected {other:?}"),
        }
    }
}
