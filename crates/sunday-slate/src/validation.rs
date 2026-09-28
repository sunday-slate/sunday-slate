use regex::Regex;
use std::sync::LazyLock;

/// Rejects strings containing any whitespace (spaces, tabs, newlines, NBSP, etc.).
pub(crate) fn no_whitespace(value: &str, _: &()) -> garde::Result {
    if value.contains(char::is_whitespace) {
        return Err(garde::Error::new("must not contain spaces"));
    }
    Ok(())
}

/// Letters, marks, digits, spaces, and ' - . , & — the allowed character set for
/// user-facing names (leagues, teams, owners). Blocks emoji and other symbols.
static NAME_CHARS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\p{L}\p{M}\p{N} '.,&-]+$").unwrap());

/// Rejects names containing characters outside [`NAME_CHARS`].
pub(crate) fn valid_name_chars(value: &str, _: &()) -> garde::Result {
    if NAME_CHARS.is_match(value) {
        return Ok(());
    }
    Err(garde::Error::new("invalid characters"))
}

/// Trim a name and collapse every internal run of whitespace to a single space.
/// Applied before validation and insert so stored names never contain tabs,
/// newlines, or repeated spaces.
pub(crate) fn normalize_name(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_whitespace_accepts_plain() {
        assert!(no_whitespace("hunter22", &()).is_ok());
    }

    #[test]
    fn no_whitespace_rejects_space() {
        let Err(e) = no_whitespace("hunter 22", &()) else {
            panic!("expected error");
        };
        assert_eq!(e.message(), "must not contain spaces");
    }

    #[test]
    fn no_whitespace_rejects_tab_and_newline() {
        assert!(no_whitespace("hunter\t22", &()).is_err());
        assert!(no_whitespace("hunter\n22", &()).is_err());
    }

    #[test]
    fn no_whitespace_rejects_non_breaking_space() {
        assert!(no_whitespace("hunter\u{00A0}22", &()).is_err());
    }

    #[test]
    fn normalize_name_trims_and_collapses() {
        assert_eq!(super::normalize_name("  John   Doe  "), "John Doe");
        assert_eq!(super::normalize_name("John\tDoe"), "John Doe");
        assert_eq!(super::normalize_name("John\nDoe"), "John Doe");
        assert_eq!(super::normalize_name("Mike"), "Mike");
        assert_eq!(super::normalize_name("   "), "");
    }
}
