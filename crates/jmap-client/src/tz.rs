//! Time zone conversion for JSCalendar's `LocalDateTime` + `timeZone`
//! pair (RFC 8984 §1.4.3-4): a wall-clock time with no zone is "floating"
//! and displays identically in any zone; otherwise it's the wall-clock
//! time in the named IANA zone.

use std::str::FromStr;

use chrono::NaiveDateTime;
use chrono::TimeZone as _;
pub use chrono_tz::Tz;

pub fn parse_tz(name: &str) -> Option<Tz> {
    Tz::from_str(name).ok()
}

/// Reinterpret `naive` (a wall-clock time in `from`, or floating if `None`)
/// as the equivalent wall-clock time in `to`.
pub fn convert(naive: NaiveDateTime, from: Option<Tz>, to: Tz) -> NaiveDateTime {
    let Some(from_tz) = from else {
        return naive;
    };
    if from_tz == to {
        return naive;
    }
    let localized = match from_tz.from_local_datetime(&naive).earliest() {
        Some(dt) => dt,
        None => return naive,
    };
    localized.with_timezone(&to).naive_local()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn converts_across_zones_in_summer() {
        let berlin: Tz = "Europe/Berlin".parse().unwrap();
        let ny: Tz = "America/New_York".parse().unwrap();
        let naive = NaiveDate::from_ymd_opt(2026, 7, 15)
            .unwrap()
            .and_hms_opt(18, 0, 0)
            .unwrap();
        // Berlin is UTC+2, New York is UTC-4 in July -> 6 hour difference.
        let converted = convert(naive, Some(berlin), ny);
        assert_eq!(
            converted,
            NaiveDate::from_ymd_opt(2026, 7, 15).unwrap().and_hms_opt(12, 0, 0).unwrap()
        );
    }

    #[test]
    fn floating_time_is_unchanged() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let naive = NaiveDate::from_ymd_opt(2026, 7, 15).unwrap().and_hms_opt(9, 0, 0).unwrap();
        assert_eq!(convert(naive, None, ny), naive);
    }
}
