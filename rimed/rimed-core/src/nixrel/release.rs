//! Release IDs: `YYYY.MM.DD` for the first release of a day, `YYYY.MM.DD.N` (N >= 2) after it
//! (files/scripts/next-release-id). Compared field by field as integers, so `2026.12.03.10` is newer than
//! `2026.12.03.9`.

use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReleaseId {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    /// 1 for the bare date form.
    pub n: u32,
}

impl ReleaseId {
    pub fn parse(s: &str) -> Option<ReleaseId> {
        let parts: Vec<&str> = s.split('.').collect();
        if !(3..=4).contains(&parts.len()) || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
            return None;
        }
        if parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
            return None;
        }
        let year = parts[0].parse().ok()?;
        let month: u32 = parts[1].parse().ok()?;
        let day: u32 = parts[2].parse().ok()?;
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        let n = match parts.get(3) {
            None => 1,
            Some(p) => {
                // "2026.12.03.1" is never written (the first release has no suffix), and a leading zero would make
                // two spellings for one ID.
                if p.starts_with('0') {
                    return None;
                }
                let n: u32 = p.parse().ok()?;
                if n < 2 {
                    return None;
                }
                n
            }
        };
        Some(ReleaseId { year, month, day, n })
    }
}

/// The order of two release ID strings; `None` when either does not parse.
pub fn compare(a: &str, b: &str) -> Option<Ordering> {
    Some(ReleaseId::parse(a)?.cmp(&ReleaseId::parse(b)?))
}

/// The newest of a set of release IDs (unparseable ones ignored).
pub fn newest<'a>(ids: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    ids.into_iter().filter_map(|s| ReleaseId::parse(s).map(|r| (r, s))).max_by_key(|(r, _)| *r).map(|(_, s)| s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_not_lexical_order() {
        assert_eq!(compare("2026.12.03.10", "2026.12.03.9"), Some(Ordering::Greater));
        assert_eq!(compare("2026.12.03", "2026.12.03.2"), Some(Ordering::Less));
        assert_eq!(compare("2026.11.30.5", "2026.12.01"), Some(Ordering::Less));
        assert_eq!(compare("2026.12.03", "2026.12.03"), Some(Ordering::Equal));
        assert_eq!(newest(["2026.12.03.9", "2026.12.03.10", "junk", "2026.12.02"]), Some("2026.12.03.10"));
    }

    #[test]
    fn malformed_ids_do_not_parse() {
        for bad in ["", "2026.12", "2026.12.03.1", "2026.12.03.02", "2026.13.01", "26.12.03", "2026.12.03.x", "2026.12.03.2.1"] {
            assert!(ReleaseId::parse(bad).is_none(), "{bad}");
        }
    }
}
