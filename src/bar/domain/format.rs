//! The bar's words: a cut title, the date, the time, and when the time next changes.

use std::time::Duration;

/// Cut to `max` characters, not bytes, marking the cut with an ellipsis.
///
/// Titles carry em dashes, bullets and CJK. A byte cut both mismeasures them and can split a
/// codepoint, which draws as a replacement box.
pub fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_string(),
        Some((cut, _)) => format!("{}…", &text[..cut]),
    }
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// "Tue 29th". `weekday` counts from Sunday = 0, as `tm_wday` does.
///
/// An ordinal and no month: the month is redundant beside a weekday you already know, and a
/// zero-padded numeral reads as data where this is prose. The weekday keeps its capital; lower-cased
/// it looked like a truncated log line.
pub fn date_label(weekday: u32, day: u32) -> String {
    let suffix = match (day % 100, day % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{} {day}{suffix}", WEEKDAYS[(weekday % 7) as usize])
}

/// "17:54". 24-hour, and no seconds: seconds move once a second, and nothing on the bar may move at
/// rest.
pub fn clock_label(hour: u32, minute: u32) -> String {
    format!("{hour:02}:{minute:02}")
}

/// How long until the minute turns over, from how far into the minute it is.
///
/// A timer on a fixed 30s period, which is what the bar used before, shows the old minute for up to
/// half a minute. This lands on the boundary.
pub fn until_next_minute(seconds_into_minute: f64) -> Duration {
    let into = seconds_into_minute.clamp(0.0, 60.0);
    Duration::from_secs_f64((60.0 - into).max(0.001))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_title_is_left_alone() {
        assert_eq!(truncate("lib.rs — rini", 48), "lib.rs — rini");
        assert_eq!(truncate("", 48), "");
    }

    /// Counted in characters: an em dash is three bytes and one character.
    #[test]
    fn a_long_title_is_cut_by_characters() {
        assert_eq!(truncate("ab—cd", 3), "ab—…");
        assert_eq!(truncate("日本語のタイトル", 4), "日本語の…");
        assert_eq!(truncate("exactly", 7), "exactly");
    }

    #[test]
    fn dates_take_an_ordinal() {
        assert_eq!(date_label(2, 29), "Tue 29th");
        assert_eq!(date_label(5, 1), "Fri 1st");
        assert_eq!(date_label(6, 2), "Sat 2nd");
        assert_eq!(date_label(0, 3), "Sun 3rd");
        assert_eq!(date_label(1, 22), "Mon 22nd");
        assert_eq!(date_label(3, 31), "Wed 31st");
    }

    /// 11, 12 and 13 are "th" however they end.
    #[test]
    fn the_teens_are_th() {
        assert_eq!(date_label(4, 11), "Thu 11th");
        assert_eq!(date_label(4, 12), "Thu 12th");
        assert_eq!(date_label(4, 13), "Thu 13th");
    }

    #[test]
    fn the_time_is_24_hour_and_padded() {
        assert_eq!(clock_label(7, 5), "07:05");
        assert_eq!(clock_label(17, 54), "17:54");
        assert_eq!(clock_label(0, 0), "00:00");
    }

    #[test]
    fn the_next_tick_lands_on_the_minute() {
        assert_eq!(until_next_minute(0.0), Duration::from_secs(60));
        assert_eq!(until_next_minute(59.5), Duration::from_millis(500));
        assert_eq!(until_next_minute(15.25), Duration::from_secs_f64(44.75));
    }

    /// A leap second or a clock read a hair late never asks for a negative or zero wait.
    #[test]
    fn the_wait_is_never_zero() {
        assert!(until_next_minute(60.0) > Duration::ZERO);
        assert!(until_next_minute(61.0) > Duration::ZERO);
    }
}
