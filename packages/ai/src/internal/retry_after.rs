//! Retry-After header parser (HTTP-date form).
//! 翻译自 packages/ai/src/internal/retry-after.ts
//!
//! Parses the three HTTP-date forms accepted for `Retry-After` without
//! relying on `Date.parse`. Returns the absolute unix epoch in ms.

use once_cell::sync::Lazy;
use regex::Regex;

const HTTP_DATE_MONTH_INDEX: &[(&str, u32)] = &[
    ("Jan", 0),
    ("Feb", 1),
    ("Mar", 2),
    ("Apr", 3),
    ("May", 4),
    ("Jun", 5),
    ("Jul", 6),
    ("Aug", 7),
    ("Sep", 8),
    ("Oct", 9),
    ("Nov", 10),
    ("Dec", 11),
];

fn month_index(name: &str) -> Option<u32> {
    HTTP_DATE_MONTH_INDEX
        .iter()
        .find_map(|(k, v)| if *k == name { Some(*v) } else { None })
}

fn short_weekday_index(name: &str) -> Option<u32> {
    const SHORT: &[&str] = &["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    SHORT.iter().position(|w| *w == name).map(|i| i as u32)
}

fn long_weekday_index(name: &str) -> Option<u32> {
    const LONG: &[&str] = &[
        "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
    ];
    LONG.iter().position(|w| *w == name).map(|i| i as u32)
}

struct HttpDateComponents {
    weekday: Option<u32>,
    year: i32,
    month: Option<u32>,
    day: u32,
    hours: u32,
    minutes: u32,
    seconds: u32,
}

static IMF_FIXDATE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(Mon|Tue|Wed|Thu|Fri|Sat|Sun), (\d{2}) (Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) (\d{4}) (\d{2}):(\d{2}):(\d{2}) GMT$").unwrap()
});

static OBSOLETE_RFC850_DATE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(Monday|Tuesday|Wednesday|Thursday|Friday|Saturday|Sunday), (\d{2})-(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)-(\d{2}) (\d{2}):(\d{2}):(\d{2}) GMT$").unwrap()
});

static OBSOLETE_ASCTIME_DATE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(Mon|Tue|Wed|Thu|Fri|Sat|Sun) (Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) (\d{2}| \d) (\d{2}):(\d{2}):(\d{2}) (\d{4})$").unwrap()
});

fn parse_http_date_calendar_ms(
    year: i32,
    month: Option<u32>,
    day: u32,
    hours: u32,
    minutes: u32,
    seconds: u32,
) -> Option<i64> {
    let month = month?;
    if !Number_is_integer_i32(year)
        || year < 1900
        || day < 1
        || day > 31
        || hours > 23
        || minutes > 59
        || seconds > 60
    {
        return None;
    }
    let calendar_second = std::cmp::min(seconds, 59);
    let ts = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
        chrono::NaiveDate::from_ymd_opt(year, month + 1, day)?
            .and_hms_opt(hours, minutes, calendar_second)?,
        chrono::Utc,
    )
    .timestamp_millis();
    let parsed = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ts)?;
    if parsed.format("%Y").to_string().parse::<i32>().ok()? != year
        || parsed.format("%m").to_string().parse::<u32>().ok()? != month + 1
        || parsed.format("%d").to_string().parse::<u32>().ok()? != day
        || parsed.format("%H").to_string().parse::<u32>().ok()? != hours
        || parsed.format("%M").to_string().parse::<u32>().ok()? != minutes
        || parsed.format("%S").to_string().parse::<u32>().ok()? != calendar_second
    {
        return None;
    }
    if seconds == 60 {
        Some(ts + 1_000)
    } else {
        Some(ts)
    }
}

#[allow(non_snake_case)]
fn Number_is_integer_i32(_year: i32) -> bool {
    true
}

fn parse_http_date_components_ms(c: &HttpDateComponents) -> Option<i64> {
    let timestamp = parse_http_date_calendar_ms(c.year, c.month, c.day, c.hours, c.minutes, c.seconds)?;
    let weekday_ts = if c.seconds == 60 { timestamp - 1_000 } else { timestamp };
    let parsed = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(weekday_ts)?;
    let weekday = parsed.format("%w").to_string().parse::<u32>().ok()?;
    if let Some(expected) = c.weekday {
        if weekday != expected {
            return None;
        }
    }
    Some(timestamp)
}

/// Parses the three HTTP-date forms accepted for `Retry-After`.
pub fn parse_retry_after_http_date_ms(value: &str, now_ms: Option<i64>) -> Option<i64> {
    let now_ms = now_ms.unwrap_or_else(|| chrono::Utc::now().timestamp_millis());

    if let Some(caps) = IMF_FIXDATE_RE.captures(value) {
        let weekday = short_weekday_index(caps.get(1)?.as_str());
        let year = caps.get(4)?.as_str().parse::<i32>().ok()?;
        let month = month_index(caps.get(3)?.as_str());
        let day = caps.get(2)?.as_str().parse::<u32>().ok()?;
        let hours = caps.get(5)?.as_str().parse::<u32>().ok()?;
        let minutes = caps.get(6)?.as_str().parse::<u32>().ok()?;
        let seconds = caps.get(7)?.as_str().parse::<u32>().ok()?;
        return parse_http_date_components_ms(&HttpDateComponents {
            weekday,
            year,
            month,
            day,
            hours,
            minutes,
            seconds,
        });
    }

    if let Some(caps) = OBSOLETE_RFC850_DATE_RE.captures(value) {
        let now = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(now_ms)?;
        let short_year = caps.get(4)?.as_str().parse::<i32>().ok()?;
        let candidate_year = (now.format("%Y").to_string().parse::<i32>().ok()? / 100) * 100 + short_year;
        let weekday = long_weekday_index(caps.get(1)?.as_str());
        let month = month_index(caps.get(3)?.as_str());
        let day = caps.get(2)?.as_str().parse::<u32>().ok()?;
        let hours = caps.get(5)?.as_str().parse::<u32>().ok()?;
        let minutes = caps.get(6)?.as_str().parse::<u32>().ok()?;
        let seconds = caps.get(7)?.as_str().parse::<u32>().ok()?;
        let candidate = parse_http_date_calendar_ms(candidate_year, month, day, hours, minutes, seconds)?;
        let fifty = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
            chrono::NaiveDate::from_ymd_opt(
                now.format("%Y").to_string().parse::<i32>().ok()? + 50,
                now.format("%m").to_string().parse::<u32>().ok()?,
                now.format("%d").to_string().parse::<u32>().ok()?,
            )?
            .and_hms_opt(
                now.format("%H").to_string().parse::<u32>().ok()?,
                now.format("%M").to_string().parse::<u32>().ok()?,
                now.format("%S").to_string().parse::<u32>().ok()?,
            )?,
            chrono::Utc,
        )
        .timestamp_millis();
        let resolved_year = if candidate > fifty {
            candidate_year - 100
        } else {
            candidate_year
        };
        return parse_http_date_components_ms(&HttpDateComponents {
            weekday,
            year: resolved_year,
            month,
            day,
            hours,
            minutes,
            seconds,
        });
    }

    if let Some(caps) = OBSOLETE_ASCTIME_DATE_RE.captures(value) {
        let weekday = short_weekday_index(caps.get(1)?.as_str());
        let month = month_index(caps.get(2)?.as_str());
        let day = caps.get(3)?.as_str().trim().parse::<u32>().ok()?;
        let hours = caps.get(4)?.as_str().parse::<u32>().ok()?;
        let minutes = caps.get(5)?.as_str().parse::<u32>().ok()?;
        let seconds = caps.get(6)?.as_str().parse::<u32>().ok()?;
        let year = caps.get(7)?.as_str().parse::<i32>().ok()?;
        return parse_http_date_components_ms(&HttpDateComponents {
            weekday,
            year,
            month,
            day,
            hours,
            minutes,
            seconds,
        });
    }

    None
}