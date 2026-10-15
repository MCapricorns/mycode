//! Local-calendar date grouping and relative-time labels for sidebar sessions.
//!
//! Pure formatting and classification helpers accept an explicit timezone and
//! reference instant so calendar boundaries (midnight, yesterday, 7-day
//! rollovers, and future clock skew) can be tested deterministically.

use chrono::{DateTime, Datelike as _, Local, TimeZone, Utc};

use crate::i18n::{is_chinese, t};

/// Calendar group a sidebar session belongs to, in display order from newest
/// to oldest (with [`Self::Unknown`] at the tail for missing or corrupt
/// timestamps).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum DateGroup {
    Today,
    Yesterday,
    Last7Days,
    Older,
    Unknown,
}

impl DateGroup {
    /// Ordered list of date buckets rendered in the sidebar.
    pub(super) const ALL: [Self; 5] = [
        Self::Today,
        Self::Yesterday,
        Self::Last7Days,
        Self::Older,
        Self::Unknown,
    ];

    /// Stable element-id slug for this group header.
    #[must_use]
    pub(super) const fn id_slug(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Yesterday => "yesterday",
            Self::Last7Days => "last-7-days",
            Self::Older => "older",
            Self::Unknown => "unknown",
        }
    }

    /// Localized group heading shown above the group's session rows.
    #[must_use]
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Today => t("Today", "今天"),
            Self::Yesterday => t("Yesterday", "昨天"),
            Self::Last7Days => t("Last 7 days", "最近 7 天"),
            Self::Older => t("Older", "更早"),
            Self::Unknown => t("Unknown date", "未知时间"),
        }
    }
}

/// Classifies a session timestamp against the local wall clock.
///
/// Corrupt rows or missing/zero timestamps map to [`DateGroup::Unknown`]
/// rather than fabricating `Today`.
#[must_use]
pub(super) fn classify_session_group(
    updated_at_ms: Option<u64>,
    corrupt: bool,
    now: DateTime<Local>,
) -> DateGroup {
    if corrupt {
        return DateGroup::Unknown;
    }
    classify_session_group_in_tz(updated_at_ms, &now)
}

/// Formats a compact relative-time label for a session row, or `None` when
/// the timestamp is unknown or the row is corrupt.
#[must_use]
pub(super) fn format_session_relative_time(
    updated_at_ms: Option<u64>,
    corrupt: bool,
    now: DateTime<Local>,
) -> Option<String> {
    if corrupt {
        return None;
    }
    format_relative_time_in_tz(updated_at_ms, &now, is_chinese())
}

fn decode_utc(updated_at_ms: Option<u64>) -> Option<DateTime<Utc>> {
    let ms = updated_at_ms.filter(|&ms| ms > 0)?;
    let ms_i64 = i64::try_from(ms).ok()?;
    DateTime::<Utc>::from_timestamp_millis(ms_i64)
}

fn classify_session_group_in_tz<Tz: TimeZone>(
    updated_at_ms: Option<u64>,
    now: &DateTime<Tz>,
) -> DateGroup {
    let Some(stamp_utc) = decode_utc(updated_at_ms) else {
        return DateGroup::Unknown;
    };
    let stamp_local = stamp_utc.with_timezone(&now.timezone());
    let today = now.date_naive();
    let session_date = stamp_local.date_naive();
    let day_diff = today.signed_duration_since(session_date).num_days();
    if day_diff <= 0 {
        // Same local calendar day, or ahead of local clock due to skew.
        DateGroup::Today
    } else if day_diff == 1 {
        DateGroup::Yesterday
    } else if day_diff < 7 {
        // 2..=6 calendar days before today (within the 7-day window including today).
        DateGroup::Last7Days
    } else {
        DateGroup::Older
    }
}

fn format_relative_time_in_tz<Tz: TimeZone>(
    updated_at_ms: Option<u64>,
    now: &DateTime<Tz>,
    chinese: bool,
) -> Option<String> {
    let stamp_utc = decode_utc(updated_at_ms)?;
    let now_utc = now.with_timezone(&Utc);
    let elapsed_secs = now_utc
        .signed_duration_since(stamp_utc)
        .num_seconds()
        .max(0);

    if elapsed_secs < 60 {
        return Some(if chinese {
            "刚刚".to_owned()
        } else {
            "now".to_owned()
        });
    }
    let minutes = elapsed_secs / 60;
    if minutes < 60 {
        return Some(if chinese {
            format!("{minutes}分钟")
        } else {
            format!("{minutes}m")
        });
    }
    let hours = elapsed_secs / 3_600;
    if hours < 24 {
        return Some(if chinese {
            format!("{hours}小时")
        } else {
            format!("{hours}h")
        });
    }

    let stamp_local = stamp_utc.with_timezone(&now.timezone());
    let calendar_days = now
        .date_naive()
        .signed_duration_since(stamp_local.date_naive())
        .num_days();
    let days = calendar_days.max(elapsed_secs / 86_400).max(1);
    if days < 7 {
        return Some(if chinese {
            format!("{days}天")
        } else {
            format!("{days}d")
        });
    }

    // For sessions 7+ days old, show a compact local calendar date.
    Some(format_calendar_date(&stamp_local, now, chinese))
}

fn format_calendar_date<Tz: TimeZone>(
    stamp: &DateTime<Tz>,
    now: &DateTime<Tz>,
    chinese: bool,
) -> String {
    let same_year = stamp.year() == now.year();
    if chinese {
        if same_year {
            format!("{}月{}日", stamp.month(), stamp.day())
        } else {
            format!("{}/{}/{}", stamp.year(), stamp.month(), stamp.day())
        }
    } else {
        let month = english_month_short(stamp.month());
        if same_year {
            format!("{month} {}", stamp.day())
        } else {
            format!("{month} {}, {}", stamp.day(), stamp.year())
        }
    }
}

const fn english_month_short(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, TimeZone as _};

    use super::{DateGroup, classify_session_group_in_tz, format_relative_time_in_tz};

    fn tz_plus_8() -> FixedOffset {
        FixedOffset::east_opt(8 * 3_600).expect("UTC+8")
    }

    fn ms(tz: FixedOffset, year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> u64 {
        let dt = tz
            .with_ymd_and_hms(year, month, day, hour, min, sec)
            .single()
            .expect("valid local datetime");
        u64::try_from(dt.timestamp_millis()).expect("post-epoch ms")
    }

    #[test]
    fn unknown_and_zero_timestamps_never_masquerade_as_today() {
        let tz = tz_plus_8();
        let now = tz
            .with_ymd_and_hms(2026, 4, 15, 14, 30, 0)
            .single()
            .expect("now");
        assert_eq!(classify_session_group_in_tz(None, &now), DateGroup::Unknown);
        assert_eq!(
            classify_session_group_in_tz(Some(0), &now),
            DateGroup::Unknown
        );
        assert_eq!(format_relative_time_in_tz(None, &now, false), None);
        assert_eq!(format_relative_time_in_tz(Some(0), &now, true), None);
    }

    #[test]
    fn local_midnight_boundary_separates_today_and_yesterday() {
        let tz = tz_plus_8();
        // Local time is 2026-04-15 00:05:00+08:00 (still 2026-04-14 in UTC).
        let now = tz
            .with_ymd_and_hms(2026, 4, 15, 0, 5, 0)
            .single()
            .expect("now");

        // 2026-04-15 00:00:00+08:00 -> Today
        let at_midnight = ms(tz, 2026, 4, 15, 0, 0, 0);
        assert_eq!(
            classify_session_group_in_tz(Some(at_midnight), &now),
            DateGroup::Today
        );

        // 2026-04-14 23:59:59+08:00 -> Yesterday (even though only 5m01s ago)
        let just_before_midnight = ms(tz, 2026, 4, 14, 23, 59, 59);
        assert_eq!(
            classify_session_group_in_tz(Some(just_before_midnight), &now),
            DateGroup::Yesterday
        );
        assert_eq!(
            format_relative_time_in_tz(Some(just_before_midnight), &now, false).as_deref(),
            Some("5m")
        );
    }

    #[test]
    fn seven_day_and_older_calendar_boundaries_are_exact() {
        let tz = tz_plus_8();
        let now = tz
            .with_ymd_and_hms(2026, 4, 15, 18, 0, 0)
            .single()
            .expect("now");

        // 1 calendar day ago -> Yesterday
        let one_day_ago = ms(tz, 2026, 4, 14, 23, 0, 0);
        assert_eq!(
            classify_session_group_in_tz(Some(one_day_ago), &now),
            DateGroup::Yesterday
        );

        // 2 calendar days ago -> Last 7 days
        let two_days_ago = ms(tz, 2026, 4, 13, 23, 59, 0);
        assert_eq!(
            classify_session_group_in_tz(Some(two_days_ago), &now),
            DateGroup::Last7Days
        );

        // 6 calendar days ago (start of the 7-day window including today) -> Last 7 days
        let six_days_ago = ms(tz, 2026, 4, 9, 0, 0, 0);
        assert_eq!(
            classify_session_group_in_tz(Some(six_days_ago), &now),
            DateGroup::Last7Days
        );

        // 7 calendar days ago -> Older
        let seven_days_ago = ms(tz, 2026, 4, 8, 23, 59, 59);
        assert_eq!(
            classify_session_group_in_tz(Some(seven_days_ago), &now),
            DateGroup::Older
        );
    }

    #[test]
    fn future_timestamps_clamp_to_today_and_now() {
        let tz = tz_plus_8();
        let now = tz
            .with_ymd_and_hms(2026, 4, 15, 12, 0, 0)
            .single()
            .expect("now");
        let future = ms(tz, 2026, 4, 16, 9, 0, 0);
        assert_eq!(
            classify_session_group_in_tz(Some(future), &now),
            DateGroup::Today
        );
        assert_eq!(
            format_relative_time_in_tz(Some(future), &now, false).as_deref(),
            Some("now")
        );
        assert_eq!(
            format_relative_time_in_tz(Some(future), &now, true).as_deref(),
            Some("刚刚")
        );
    }

    #[test]
    fn relative_time_scales_from_seconds_through_calendar_dates() {
        let tz = tz_plus_8();
        let now = tz
            .with_ymd_and_hms(2026, 4, 15, 15, 0, 0)
            .single()
            .expect("now");

        let thirty_secs = ms(tz, 2026, 4, 15, 14, 59, 30);
        assert_eq!(
            format_relative_time_in_tz(Some(thirty_secs), &now, false).as_deref(),
            Some("now")
        );
        assert_eq!(
            format_relative_time_in_tz(Some(thirty_secs), &now, true).as_deref(),
            Some("刚刚")
        );

        let twelve_mins = ms(tz, 2026, 4, 15, 14, 48, 0);
        assert_eq!(
            format_relative_time_in_tz(Some(twelve_mins), &now, false).as_deref(),
            Some("12m")
        );
        assert_eq!(
            format_relative_time_in_tz(Some(twelve_mins), &now, true).as_deref(),
            Some("12分钟")
        );

        let three_hours = ms(tz, 2026, 4, 15, 12, 0, 0);
        assert_eq!(
            format_relative_time_in_tz(Some(three_hours), &now, false).as_deref(),
            Some("3h")
        );
        assert_eq!(
            format_relative_time_in_tz(Some(three_hours), &now, true).as_deref(),
            Some("3小时")
        );

        let four_days = ms(tz, 2026, 4, 11, 15, 0, 0);
        assert_eq!(
            format_relative_time_in_tz(Some(four_days), &now, false).as_deref(),
            Some("4d")
        );
        assert_eq!(
            format_relative_time_in_tz(Some(four_days), &now, true).as_deref(),
            Some("4天")
        );

        let same_year_old = ms(tz, 2026, 3, 2, 10, 0, 0);
        assert_eq!(
            format_relative_time_in_tz(Some(same_year_old), &now, false).as_deref(),
            Some("Mar 2")
        );
        assert_eq!(
            format_relative_time_in_tz(Some(same_year_old), &now, true).as_deref(),
            Some("3月2日")
        );

        let prior_year = ms(tz, 2025, 12, 31, 22, 0, 0);
        assert_eq!(
            format_relative_time_in_tz(Some(prior_year), &now, false).as_deref(),
            Some("Dec 31, 2025")
        );
        assert_eq!(
            format_relative_time_in_tz(Some(prior_year), &now, true).as_deref(),
            Some("2025/12/31")
        );
    }
}
