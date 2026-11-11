//! Ordering of version strings.

use std::cmp::Ordering;

/// Returns the highest version, comparing digit runs numerically.
pub fn max<'a>(versions: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    versions.max_by(|a, b| compare(a, b))
}

fn compare(a: &str, b: &str) -> Ordering {
    let (mut lhs, mut rhs) = (chunks(a), chunks(b));
    loop {
        let ordering = match (lhs.next(), rhs.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(l), Some(r)) => match (l.parse::<u64>(), r.parse::<u64>()) {
                (Ok(l), Ok(r)) => l.cmp(&r),
                _ => l.cmp(r),
            },
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
}

fn chunks(mut s: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        let first = s.chars().next()?;
        let end = s
            .find(|c: char| c.is_ascii_digit() != first.is_ascii_digit())
            .unwrap_or(s.len());
        let (chunk, rest) = s.split_at(end);
        s = rest;
        Some(chunk)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_numbers_numerically() {
        assert_eq!(compare("1.10.0", "1.9.0"), Ordering::Greater);
        assert_eq!(compare("1.2", "1.2.1"), Ordering::Less);
        assert_eq!(compare("v2", "v2"), Ordering::Equal);
        assert_eq!(max(["0.9", "0.10", "0.2"].into_iter()), Some("0.10"));
    }
}
