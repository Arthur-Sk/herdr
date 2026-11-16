//! TODO marks on panes: a person marks an agent to come back to, optionally with
//! a reminder time, and clears the mark by hand. This module holds the pure parts
//! shared by the server, the CLI and the TUI: reading the reminder time a person
//! types, showing it back, and the reserved agent tokens that carry the mark to
//! the TUI without changing the frozen client-shell wire codec.

use std::time::{SystemTime, UNIX_EPOCH};

/// Agent token set while the pane is marked TODO. The value is unused ("1").
pub(crate) const TODO_TOKEN: &str = "herdr.todo";
/// Agent token holding a pending reminder as Unix seconds.
pub(crate) const REMIND_AT_TOKEN: &str = "herdr.todo.remind_at";

/// Longest reminder delay accepted, so a typo like "300d" is refused.
const MAX_DELAY_SECS: u64 = 30 * 24 * 60 * 60;

pub(crate) const REMIND_INPUT_HINT: &str = "30m, 2h, 1h30m, 1d or 18:00";

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// The local time zone's offset from UTC in seconds, read from the platform's
/// local clock. Zones are whole quarter hours, which absorbs the second that may
/// pass between the two clock reads.
pub(crate) fn local_utc_offset_secs() -> i64 {
    let Some(local) = crate::platform::local_datetime() else {
        return 0;
    };
    let utc = time::OffsetDateTime::now_utc().unix_timestamp();
    let difference = local.assume_utc().unix_timestamp() - utc;
    (difference as f64 / 900.0).round() as i64 * 900
}

/// `local_utc_offset_secs`, read again at most every ten minutes: the TUI shows
/// reminder times on every frame, and a zone change (DST) only needs to land soon.
pub(crate) fn cached_local_utc_offset_secs() -> i64 {
    static CACHE: std::sync::Mutex<Option<(u64, i64)>> = std::sync::Mutex::new(None);
    let now = unix_now();
    let Ok(mut cache) = CACHE.lock() else {
        return local_utc_offset_secs();
    };
    match *cache {
        Some((read_at, offset)) if now.saturating_sub(read_at) < 600 => offset,
        _ => {
            let offset = local_utc_offset_secs();
            *cache = Some((now, offset));
            offset
        }
    }
}

/// Reads a reminder time: a delay ("30m", "2h", "1h30m", "1d", "90s", a bare
/// number of minutes; Russian ч/м/д/с units work too) or a local clock time
/// ("18:00", "9.05"), which means its next occurrence. Returns Unix seconds.
pub(crate) fn parse_remind_at(input: &str, now: u64, utc_offset_secs: i64) -> Result<u64, String> {
    let text = input.trim().to_lowercase();
    if text.is_empty() {
        return Err(format!("enter when to remind: {REMIND_INPUT_HINT}"));
    }
    if let Some(at) = parse_clock_time(&text, now, utc_offset_secs) {
        return Ok(at);
    }
    let delay = parse_delay(&text)
        .ok_or_else(|| format!("cannot read {input:?}: use {REMIND_INPUT_HINT}"))?;
    if delay == 0 || delay > MAX_DELAY_SECS {
        return Err("a reminder must be between 1 second and 30 days away".into());
    }
    Ok(now + delay)
}

fn parse_clock_time(text: &str, now: u64, utc_offset_secs: i64) -> Option<u64> {
    let (hours, minutes) = text.split_once(':').or_else(|| text.split_once('.'))?;
    let hours = parse_digits(hours, 2)?;
    let minutes = parse_digits(minutes, 2)?;
    if hours > 23 || minutes > 59 || minutes_text_len(text) != 2 {
        return None;
    }
    let local_now = now as i64 + utc_offset_secs;
    let day_start = local_now.div_euclid(86_400) * 86_400;
    let mut local_at = day_start + (hours * 3600 + minutes * 60) as i64;
    if local_at <= local_now {
        local_at += 86_400;
    }
    u64::try_from(local_at - utc_offset_secs).ok()
}

fn minutes_text_len(text: &str) -> usize {
    text.rsplit([':', '.']).next().map_or(0, str::len)
}

fn parse_digits(text: &str, max_len: usize) -> Option<u64> {
    (!text.is_empty() && text.len() <= max_len && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

fn parse_delay(text: &str) -> Option<u64> {
    if text.bytes().all(|byte| byte.is_ascii_digit()) {
        return text.parse::<u64>().ok()?.checked_mul(60);
    }
    let mut total = 0_u64;
    let mut rest = text.trim();
    while !rest.is_empty() {
        let digits_end = rest
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits_end == 0 {
            return None;
        }
        let amount = rest[..digits_end].parse::<u64>().ok()?;
        rest = rest[digits_end..].trim_start();
        let unit_end = rest
            .find(|character: char| character.is_ascii_digit() || character.is_whitespace())
            .unwrap_or(rest.len());
        let unit = unit_secs(&rest[..unit_end])?;
        rest = rest[unit_end..].trim_start();
        total = total.checked_add(amount.checked_mul(unit)?)?;
    }
    Some(total)
}

fn unit_secs(unit: &str) -> Option<u64> {
    Some(match unit {
        "s" | "sec" | "secs" | "с" | "сек" => 1,
        "m" | "min" | "mins" | "м" | "мин" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" | "ч" | "час" | "часа" | "часов" => 3600,
        "d" | "day" | "days" | "д" | "дн" | "день" | "дня" | "дней" => 86_400,
        _ => return None,
    })
}

/// Shows a reminder time in local time: "14:30" today, "Tue 09:00" within the
/// next week, "24.11 09:00" later.
pub(crate) fn format_remind_at(at: u64, now: u64, utc_offset_secs: i64) -> String {
    let local_at = at as i64 + utc_offset_secs;
    let local_now = now as i64 + utc_offset_secs;
    let day = local_at.div_euclid(86_400);
    let today = local_now.div_euclid(86_400);
    let seconds = local_at.rem_euclid(86_400);
    let clock = format!("{:02}:{:02}", seconds / 3600, seconds % 3600 / 60);
    if day <= today {
        return clock;
    }
    let Ok(date) = time::OffsetDateTime::from_unix_timestamp(local_at) else {
        return clock;
    };
    if day - today < 7 {
        let weekday = date.weekday().to_string();
        format!("{} {clock}", &weekday[..3])
    } else {
        format!("{:02}.{:02} {clock}", date.day(), u8::from(date.month()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-16 10:00:00 UTC, a Monday.
    const NOW: u64 = 1_794_823_200;

    #[test]
    fn parses_delays() {
        assert_eq!(parse_remind_at("30m", NOW, 0), Ok(NOW + 1800));
        assert_eq!(parse_remind_at("2h", NOW, 0), Ok(NOW + 7200));
        assert_eq!(parse_remind_at("1h30m", NOW, 0), Ok(NOW + 5400));
        assert_eq!(parse_remind_at(" 1h 30m ", NOW, 0), Ok(NOW + 5400));
        assert_eq!(parse_remind_at("1d", NOW, 0), Ok(NOW + 86_400));
        assert_eq!(parse_remind_at("45", NOW, 0), Ok(NOW + 2700));
        assert_eq!(parse_remind_at("90s", NOW, 0), Ok(NOW + 90));
        assert_eq!(parse_remind_at("2 hours", NOW, 0), Ok(NOW + 7200));
        assert_eq!(parse_remind_at("2ч", NOW, 0), Ok(NOW + 7200));
        assert_eq!(parse_remind_at("1ч 15мин", NOW, 0), Ok(NOW + 4500));
        assert_eq!(parse_remind_at("3H", NOW, 0), Ok(NOW + 10_800));
    }

    #[test]
    fn rejects_unreadable_and_out_of_range_delays() {
        for input in [
            "", "  ", "soon", "h", "1x", "0", "0m", "31d", "1h-5m", "25:00", "9:5",
        ] {
            assert!(parse_remind_at(input, NOW, 0).is_err(), "{input:?} parsed");
        }
    }

    #[test]
    fn clock_times_mean_their_next_local_occurrence() {
        // Riga in winter is UTC+2: local now is 12:00.
        let offset = 2 * 3600;
        assert_eq!(parse_remind_at("18:00", NOW, offset), Ok(NOW + 6 * 3600));
        assert_eq!(parse_remind_at("12:30", NOW, offset), Ok(NOW + 1800));
        assert_eq!(
            parse_remind_at("9.05", NOW, offset),
            Ok(NOW + 21 * 3600 + 300)
        );
        // The current minute is already past: tomorrow.
        assert_eq!(parse_remind_at("12:00", NOW, offset), Ok(NOW + 86_400));
    }

    #[test]
    fn formats_reminders_in_local_time() {
        let offset = 2 * 3600;
        assert_eq!(format_remind_at(NOW + 1800, NOW, offset), "12:30");
        assert_eq!(format_remind_at(NOW + 86_400, NOW, offset), "Tue 12:00");
        assert_eq!(
            format_remind_at(NOW + 10 * 86_400, NOW, offset),
            "26.11 12:00"
        );
        // Already due: the clock time alone.
        assert_eq!(format_remind_at(NOW - 60, NOW, offset), "11:59");
    }
}
