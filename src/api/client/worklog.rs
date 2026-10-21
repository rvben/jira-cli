//! Local validation of work-log input, so a typo fails before any request
//! instead of as Jira's terse "Invalid time duration entered".

use super::ApiError;

/// Check a Jira duration such as `2h 30m`, `1d`, `1.5h` or `90` (minutes in
/// the default configuration) and return it with one space between parts.
pub(super) fn normalize_duration(input: &str) -> Result<String, ApiError> {
    let invalid = || {
        ApiError::InvalidInput(format!(
            "Invalid time {input:?}; use Jira duration units w, d, h and m (e.g. 2h 30m, 1d, 1.5h)"
        ))
    };
    let mut parts = Vec::new();
    let mut total = 0.0_f64;
    let mut rest = input.trim();
    while !rest.is_empty() {
        let number_len = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        let number = &rest[..number_len];
        if number.is_empty()
            || number.starts_with('.')
            || number.ends_with('.')
            || number.matches('.').count() > 1
        {
            return Err(invalid());
        }
        let value: f64 = number.parse().map_err(|_| invalid())?;
        rest = &rest[number_len..];
        let unit = rest.chars().next().filter(|c| !c.is_whitespace());
        let unit = match unit.map(|c| c.to_ascii_lowercase()) {
            Some(u @ ('w' | 'd' | 'h' | 'm')) => {
                rest = &rest[1..];
                Some(u)
            }
            Some(_) => return Err(invalid()),
            None => None,
        };
        parts.push(match unit {
            Some(u) => format!("{number}{u}"),
            None => number.to_owned(),
        });
        total += value;
        rest = rest.trim_start();
    }
    // A bare number is only unambiguous on its own.
    if parts.is_empty()
        || (parts.len() > 1
            && parts
                .iter()
                .any(|p| p.ends_with(|c: char| c.is_ascii_digit())))
    {
        return Err(invalid());
    }
    if total <= 0.0 {
        return Err(ApiError::InvalidInput(format!(
            "Time {input:?} is zero; log a positive duration"
        )));
    }
    Ok(parts.join(" "))
}

/// Convert an ISO-8601 timestamp with a time zone (`2024-01-15T09:00Z`,
/// `2024-01-15T09:00:00+01:00`, `2024-01-15 09:00:00.000+0100`) into the
/// `yyyy-MM-dd'T'HH:mm:ss.SSSZ` form Jira's worklog API requires.
pub(super) fn normalize_started(input: &str) -> Result<String, ApiError> {
    let invalid = |why: &str| {
        ApiError::InvalidInput(format!(
            "Invalid --started {input:?}: {why}; use e.g. 2024-01-15T09:00+01:00 or 2024-01-15T09:00:00Z"
        ))
    };
    let s = input.trim();
    let (date, rest) = s
        .split_once(['T', 't', ' '])
        .ok_or_else(|| invalid("expected a date and a time"))?;
    let date =
        digits(date, &[4, 2, 2], '-').ok_or_else(|| invalid("expected the date as YYYY-MM-DD"))?;
    let (year, month, day) = (date[0], date[1], date[2]);
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return Err(invalid("the date does not exist"));
    }
    let zone_at = rest
        .find(['Z', 'z', '+', '-'])
        .ok_or_else(|| invalid("a time zone is required (Z or an offset like +01:00)"))?;
    let (time, zone) = rest.split_at(zone_at);
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (time, None),
    };
    let clock = digits(clock, &[2, 2], ':')
        .or_else(|| digits(clock, &[2, 2, 2], ':'))
        .ok_or_else(|| invalid("expected the time as HH:MM or HH:MM:SS"))?;
    let (hour, minute, second) = (clock[0], clock[1], clock.get(2).copied().unwrap_or(0));
    if hour > 23 || minute > 59 || second > 59 {
        return Err(invalid("the time does not exist"));
    }
    if fraction.is_some() && clock.len() < 3 {
        return Err(invalid("fractional seconds need seconds"));
    }
    let millis = match fraction {
        None => "000".to_owned(),
        Some(f) if !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()) => {
            format!("{f:0<3}")[..3].to_owned()
        }
        Some(_) => return Err(invalid("expected digits after the decimal point")),
    };
    let offset = if zone.eq_ignore_ascii_case("z") {
        "+0000".to_owned()
    } else {
        let (sign, hm) = zone.split_at(1);
        let hm = digits(hm, &[2, 2], ':')
            .or_else(|| digits(hm, &[4], ':').map(|v| vec![v[0] / 100, v[0] % 100]))
            .ok_or_else(|| invalid("expected the offset as +HH:MM or +HHMM"))?;
        if hm[0] > 14 || hm[1] > 59 {
            return Err(invalid("the offset is out of range"));
        }
        format!("{sign}{:02}{:02}", hm[0], hm[1])
    };
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis}{offset}"
    ))
}

/// Split `s` on `sep` into all-digit groups of exactly the given widths.
fn digits(s: &str, widths: &[usize], sep: char) -> Option<Vec<u32>> {
    let groups = s.split(sep).collect::<Vec<_>>();
    if groups.len() != widths.len() {
        return None;
    }
    groups
        .iter()
        .zip(widths)
        .map(|(g, &w)| {
            (g.len() == w && g.bytes().all(|b| b.is_ascii_digit()))
                .then(|| g.parse().ok())
                .flatten()
        })
        .collect()
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_checked_and_spaced() {
        assert_eq!(normalize_duration("2h 30m").unwrap(), "2h 30m");
        assert_eq!(normalize_duration(" 2h30m ").unwrap(), "2h 30m");
        assert_eq!(normalize_duration("1W 2D").unwrap(), "1w 2d");
        assert_eq!(normalize_duration("1.5h").unwrap(), "1.5h");
        assert_eq!(normalize_duration("90").unwrap(), "90");
        for bad in ["", "2 hours", "2x", "h", "2h 30", ".5h", "1..5h", "-1h"] {
            assert!(normalize_duration(bad).is_err(), "{bad:?}");
        }
        let err = normalize_duration("0h 0m").unwrap_err();
        assert!(err.to_string().contains("zero"), "{err}");
    }

    #[test]
    fn started_is_converted_to_jira_format() {
        let cases = [
            ("2024-01-15T09:00Z", "2024-01-15T09:00:00.000+0000"),
            ("2024-01-15T09:00:30+01:00", "2024-01-15T09:00:30.000+0100"),
            ("2024-01-15 09:00:00.5-0530", "2024-01-15T09:00:00.500-0530"),
            (
                "2024-01-15T09:00:00.000+0000",
                "2024-01-15T09:00:00.000+0000",
            ),
            (
                "2024-02-29T23:59:59.123456z",
                "2024-02-29T23:59:59.123+0000",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(normalize_started(input).unwrap(), expected, "{input}");
        }
    }

    #[test]
    fn started_rejects_bad_or_zoneless_timestamps() {
        let zoneless = normalize_started("2024-01-15T09:00:00").unwrap_err();
        assert!(zoneless.to_string().contains("time zone"), "{zoneless}");
        for bad in [
            "2024-01-15",
            "2023-02-29T09:00Z",
            "2024-13-01T09:00Z",
            "2024-01-15T24:00Z",
            "2024-01-15T09:00+15:00",
            "2024-01-15T09:00.5Z",
            "15-01-2024T09:00Z",
            "yesterday",
        ] {
            assert!(normalize_started(bad).is_err(), "{bad:?}");
        }
    }
}
