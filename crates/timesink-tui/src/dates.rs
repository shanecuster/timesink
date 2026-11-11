//! The small prompt for a custom range of days (the `c` key).
//!
//! You type two dates, like `2026-10-01 2026-10-31`, or one date to mean "from then until
//! today". This file only holds the typing state and the key handling, so it can be tested
//! without a terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::period::{self, Range};

/// Longest text the prompt will take (two dates with a word between fit easily).
const MAX_TEXT: usize = 40;

/// Read a range of days from what was typed.
///
/// Accepted: two dates (`2026-10-01 2026-10-31`, also separated by `to` or a comma), or one
/// date (from that day until `today`). Dates are `YYYY-MM-DD`. The two may be given in either
/// order, and an end after today is cut back to today, since nothing has been recorded yet.
pub fn parse_range(text: &str, today: i64) -> Result<Range, String> {
    // Commas and the word "to" are just separators.
    let cleaned = text.replace(',', " ").to_lowercase();
    let words: Vec<&str> = cleaned.split_whitespace().filter(|w| *w != "to").collect();
    let day = |word: &str| {
        period::parse_date(word).ok_or_else(|| format!("'{word}' is not a date like 2026-10-01"))
    };
    let (a, b) = match words.as_slice() {
        [] => return Err("type a date like 2026-10-01".to_string()),
        [one] => {
            let first = day(one)?;
            if first > today {
                return Err("that day is in the future".to_string());
            }
            (first, today)
        }
        [first, last] => (day(first)?, day(last)?),
        _ => return Err("type two dates, or one for \"until today\"".to_string()),
    };
    let range = Range::new(a, b);
    if range.first > today {
        return Err("that range starts in the future".to_string());
    }
    Ok(Range::new(range.first, range.last.min(today)))
}

/// The prompt while it is open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DatePrompt {
    /// What has been typed.
    pub text: String,
    /// Why the last attempt was refused, shown under the text.
    pub error: Option<String>,
}

impl DatePrompt {
    /// A prompt that starts with `range` filled in, ready to edit.
    pub fn starting_with(range: Range) -> DatePrompt {
        let day = |d: i64| {
            period::date_of(d)
                .map(|date| date.format("%Y-%m-%d").to_string())
                .unwrap_or_default()
        };
        DatePrompt {
            text: format!("{} {}", day(range.first), day(range.last)),
            error: None,
        }
    }
}

/// What a key press did to the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Still open (the text may have changed).
    Open,
    /// Closed without a choice.
    Cancelled,
    /// Closed with this range.
    Chosen(Range),
}

/// Apply one key press to the prompt.
pub fn handle_key(prompt: &mut DatePrompt, key: KeyEvent, today: i64) -> Outcome {
    match key.code {
        KeyCode::Esc => return Outcome::Cancelled,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return Outcome::Cancelled
        }
        KeyCode::Enter => match parse_range(&prompt.text, today) {
            Ok(range) => return Outcome::Chosen(range),
            Err(message) => prompt.error = Some(message),
        },
        KeyCode::Backspace => {
            prompt.text.pop();
            prompt.error = None;
        }
        // Only what a date range is made of, so stray keys do nothing.
        KeyCode::Char(c)
            if (c.is_ascii_digit() || matches!(c, '-' | ' ' | ',' | 't' | 'o' | 'T' | 'O'))
                && prompt.text.chars().count() < MAX_TEXT
                && !key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            prompt.text.push(c);
            prompt.error = None;
        }
        _ => {}
    }
    Outcome::Open
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain key press.
    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Type a whole string into the prompt.
    fn type_text(prompt: &mut DatePrompt, text: &str, today: i64) -> Outcome {
        let mut outcome = Outcome::Open;
        for c in text.chars() {
            outcome = handle_key(prompt, press(KeyCode::Char(c)), today);
        }
        outcome
    }

    fn d(text: &str) -> i64 {
        period::parse_date(text).unwrap()
    }

    #[test]
    fn two_dates_make_a_range_in_any_of_the_usual_spellings() {
        let today = d("2026-11-11");
        let want = Range::new(d("2026-10-01"), d("2026-10-31"));
        for text in [
            "2026-10-01 2026-10-31",
            "2026-10-01 to 2026-10-31",
            "2026-10-01, 2026-10-31",
            "  2026-10-01   TO   2026-10-31  ",
            // The wrong way round is put right.
            "2026-10-31 2026-10-01",
        ] {
            assert_eq!(parse_range(text, today), Ok(want), "{text}");
        }
    }

    #[test]
    fn one_date_means_until_today_and_the_future_is_cut_off() {
        let today = d("2026-11-11");
        assert_eq!(
            parse_range("2026-11-01", today),
            Ok(Range::new(d("2026-11-01"), today))
        );
        // An end after today is cut back; a start after today is refused.
        assert_eq!(
            parse_range("2026-11-01 2027-01-01", today),
            Ok(Range::new(d("2026-11-01"), today))
        );
        assert!(parse_range("2026-12-01", today)
            .unwrap_err()
            .contains("future"));
    }

    #[test]
    fn bad_text_gets_a_message_that_says_what_was_wrong() {
        let today = d("2026-11-11");
        assert!(parse_range("", today).unwrap_err().contains("type a date"));
        assert!(parse_range("tomorrow", today)
            .unwrap_err()
            .contains("'tomorrow' is not a date"));
        assert!(parse_range("2026-13-40", today)
            .unwrap_err()
            .contains("not a date"));
        assert!(parse_range("2026-01-01 2026-02-01 2026-03-01", today)
            .unwrap_err()
            .contains("two dates"));
    }

    #[test]
    fn typing_enter_and_escape_drive_the_prompt() {
        let today = d("2026-11-11");
        let mut p = DatePrompt::default();
        // Letters that are not part of a date do nothing.
        type_text(&mut p, "x2026-10-01 q2026-10-31", today);
        assert_eq!(p.text, "2026-10-01 2026-10-31");
        assert_eq!(
            handle_key(&mut p, press(KeyCode::Enter), today),
            Outcome::Chosen(Range::new(d("2026-10-01"), d("2026-10-31")))
        );
        assert_eq!(
            handle_key(&mut p, press(KeyCode::Esc), today),
            Outcome::Cancelled
        );
    }

    #[test]
    fn a_refused_entry_stays_open_with_a_message_until_you_edit_it() {
        let today = d("2026-11-11");
        let mut p = DatePrompt::default();
        type_text(&mut p, "2026-99-01", today);
        assert_eq!(
            handle_key(&mut p, press(KeyCode::Enter), today),
            Outcome::Open
        );
        assert!(p.error.is_some());
        // Editing clears the complaint.
        handle_key(&mut p, press(KeyCode::Backspace), today);
        assert_eq!(p.error, None);
        assert_eq!(p.text, "2026-99-0");
    }

    #[test]
    fn the_prompt_can_start_with_the_current_range_and_has_a_length_limit() {
        let p = DatePrompt::starting_with(Range::new(d("2026-10-01"), d("2026-10-31")));
        assert_eq!(p.text, "2026-10-01 2026-10-31");
        let today = d("2026-11-11");
        let mut long = DatePrompt::default();
        type_text(&mut long, &"1".repeat(100), today);
        assert_eq!(long.text.len(), MAX_TEXT);
    }
}
