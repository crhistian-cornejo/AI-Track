//! Semantic versions: just enough of semver to compare releases (`0.0.9 < 0.0.10`,
//! `1.0.0-beta.2 < 1.0.0`). Build metadata (`+…`) is ignored, as semver says.

use crate::UpdateError;
use std::cmp::Ordering;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ident {
    Num(u64),
    Alpha(String),
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            (Ident::Alpha(a), Ident::Alpha(b)) => a.cmp(b),
            // Numeric identifiers rank below alphanumeric ones.
            (Ident::Num(_), Ident::Alpha(_)) => Ordering::Less,
            (Ident::Alpha(_), Ident::Num(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pre: Vec<Ident>,
}

impl Version {
    /// `1.2.3`, `1.2.3-beta.1`, with an optional leading `v`.
    pub fn parse(text: &str) -> Result<Self, UpdateError> {
        let bad = || UpdateError::BadVersion(text.to_owned());
        let s = text.trim();
        let s = s.strip_prefix('v').unwrap_or(s);
        let s = s.split('+').next().unwrap_or(s);
        let (core, pre) = match s.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (s, None),
        };
        let mut nums = core.split('.');
        let mut next = || -> Result<u64, UpdateError> {
            let part = nums.next().ok_or_else(bad)?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad());
            }
            part.parse().map_err(|_| bad())
        };
        let (major, minor, patch) = (next()?, next()?, next()?);
        if nums.next().is_some() {
            return Err(bad());
        }
        let mut idents = Vec::new();
        if let Some(pre) = pre {
            for part in pre.split('.') {
                if part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                    return Err(bad());
                }
                idents.push(if part.bytes().all(|b| b.is_ascii_digit()) { Ident::Num(part.parse().map_err(|_| bad())?) } else { Ident::Alpha(part.to_owned()) });
            }
        }
        Ok(Self { major, minor, patch, pre: idents })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)).then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            // A release outranks its own pre-releases.
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => self.pre.cmp(&other.pre),
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        for (i, id) in self.pre.iter().enumerate() {
            f.write_str(if i == 0 { "-" } else { "." })?;
            match id {
                Ident::Num(n) => write!(f, "{n}")?,
                Ident::Alpha(a) => f.write_str(a)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn numbers_compare_as_numbers() {
        assert!(v("0.0.9") < v("0.0.10"));
        assert!(v("0.9.0") < v("0.10.0"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert_eq!(v("1.2.3"), v("v1.2.3"));
        assert_eq!(v("1.2.3+build5"), v("1.2.3"));
    }

    #[test]
    fn prereleases_order_before_the_release() {
        assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
        assert!(v("1.0.0-alpha.1") < v("1.0.0-alpha.beta"));
        assert!(v("1.0.0-alpha.beta") < v("1.0.0-beta"));
        assert!(v("1.0.0-beta") < v("1.0.0-beta.2"));
        assert!(v("1.0.0-beta.2") < v("1.0.0-beta.11"));
        assert!(v("1.0.0-beta.11") < v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.1") < v("1.0.0"));
        assert!(v("1.0.0-rc.1").is_prerelease());
    }

    #[test]
    fn nonsense_is_rejected() {
        for bad in ["", "1", "1.2", "1.2.3.4", "a.b.c", "1.2.x", "1..3", "1.2.3-", "1.2.3-a..b"] {
            assert!(Version::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn displays_without_the_prefix() {
        assert_eq!(v("v0.0.2").to_string(), "0.0.2");
        assert_eq!(v("1.0.0-rc.1").to_string(), "1.0.0-rc.1");
    }
}
