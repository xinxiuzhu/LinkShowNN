//! Shared display formats used by ordinary cards and video posters.

use chrono::{DateTime, FixedOffset, Utc};

pub(super) fn format_timestamp(timestamp: i64) -> Option<String> {
    let date = DateTime::<Utc>::from_timestamp(timestamp, 0)?;
    let timezone = FixedOffset::east_opt(8 * 60 * 60)?;
    Some(
        date.with_timezone(&timezone)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    )
}

pub(super) fn compact_count(count: u64) -> String {
    if count >= 100_000_000 {
        decimal_unit(count, 100_000_000, "亿")
    } else if count >= 10_000 {
        decimal_unit(count, 10_000, "万")
    } else {
        let raw = count.to_string();
        let mut out = String::new();
        for (index, digit) in raw.chars().enumerate() {
            if index > 0 && (raw.len() - index).is_multiple_of(3) {
                out.push(',');
            }
            out.push(digit);
        }
        out
    }
}

pub(super) fn count_text(exact: Option<u64>, display: &Option<String>) -> Option<String> {
    exact.map(compact_count).or_else(|| display.clone())
}

fn decimal_unit(count: u64, divisor: u64, unit: &str) -> String {
    let scale = divisor / 10;
    let rounded = count / scale + u64::from(count % scale >= scale / 2);
    let whole = rounded / 10;
    let tenth = rounded % 10;
    if tenth == 0 {
        format!("{whole}{unit}")
    } else {
        format!("{whole}.{tenth}{unit}")
    }
}
