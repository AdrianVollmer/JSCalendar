//! Minimal ISO 8601 duration parsing, as used by JSCalendar's `duration`
//! and `offset` properties (RFC 8984 §1.4.4): `P[n]Y[n]M[n]W[n]DT[n]H[n]M[n]S`.

use chrono::Duration;

pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.strip_prefix('-').map_or((s, false), |rest| (rest, true));
    let (body, negative) = s;
    let body = body.strip_prefix('P')?;
    let (date_part, time_part) = match body.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (body, None),
    };

    let mut total = Duration::zero();
    total += parse_component_group(date_part, &[('Y', 365), ('M', 30), ('W', 7), ('D', 1)], true)?;
    if let Some(t) = time_part {
        total += parse_component_group(t, &[('H', 0), ('M', 0), ('S', 0)], false)?;
    }

    Some(if negative { -total } else { total })
}

fn parse_component_group(s: &str, units: &[(char, i64)], is_days: bool) -> Option<Duration> {
    let mut total = Duration::zero();
    let mut num = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
            continue;
        }
        let n: i64 = num.parse().ok()?;
        num.clear();
        if is_days {
            let days = units.iter().find(|(c, _)| *c == ch)?.1 * n;
            total += Duration::days(days);
        } else {
            total += match ch {
                'H' => Duration::hours(n),
                'M' => Duration::minutes(n),
                'S' => Duration::seconds(n),
                _ => return None,
            };
        }
    }
    Some(total)
}

pub fn format_duration(d: Duration) -> String {
    if d.is_zero() {
        return "PT0S".to_string();
    }
    let negative = d < Duration::zero();
    let d = if negative { -d } else { d };
    let total_secs = d.num_seconds();
    let days = total_secs / 86400;
    let rem = total_secs % 86400;
    let hours = rem / 3600;
    let mins = (rem % 3600) / 60;
    let secs = rem % 60;

    let mut out = String::from(if negative { "-P" } else { "P" });
    if days > 0 {
        out.push_str(&format!("{days}D"));
    }
    if hours > 0 || mins > 0 || secs > 0 {
        out.push('T');
        if hours > 0 {
            out.push_str(&format!("{hours}H"));
        }
        if mins > 0 {
            out.push_str(&format!("{mins}M"));
        }
        if secs > 0 {
            out.push_str(&format!("{secs}S"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_durations() {
        assert_eq!(parse_duration("PT30M"), Some(Duration::minutes(30)));
        assert_eq!(parse_duration("PT1H30M"), Some(Duration::minutes(90)));
        assert_eq!(parse_duration("P1D"), Some(Duration::days(1)));
        assert_eq!(parse_duration("PT0S"), Some(Duration::zero()));
        assert_eq!(parse_duration("P1DT2H"), Some(Duration::hours(26)));
    }

    #[test]
    fn round_trips() {
        for s in ["PT30M", "PT1H30M", "P1D", "PT0S", "P2DT3H4M5S"] {
            let d = parse_duration(s).unwrap();
            assert_eq!(parse_duration(&format_duration(d)), Some(d));
        }
    }
}
