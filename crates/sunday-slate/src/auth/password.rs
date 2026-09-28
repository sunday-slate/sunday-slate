use crate::validation::no_whitespace;
use garde::Validate;
use serde::Deserialize;

/// A password carrying three independent validation rules:
/// ASCII-only, minimum 8 characters, and no whitespace.
#[derive(Debug, Clone, PartialEq, Deserialize, Validate)]
#[garde(transparent)]
pub struct Password(#[garde(ascii, length(min = 8), custom(no_whitespace))] pub String);
pub fn hash(plain: &str) -> String {
    password_auth::generate_hash(plain)
}

pub fn verify(plain: &str, stored_hash: &str) -> bool {
    password_auth::verify_password(plain, stored_hash).is_ok()
}

pub fn is_obsolete(stored_hash: &str) -> bool {
    password_auth::is_hash_obsolete(stored_hash).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_verify_roundtrip() {
        let hash = hash("correct horse battery staple");
        assert!(verify("correct horse battery staple", &hash));
    }

    #[test]
    fn verify_wrong_password_fails() {
        let hash = hash("hunter2");
        assert!(!verify("wrong", &hash));
    }

    #[test]
    fn verify_invalid_hash_fails() {
        assert!(!verify("anything", "not-a-valid-hash"));
    }

    #[test]
    fn fresh_hash_is_not_obsolete() {
        let hash = hash("password");
        assert!(!is_obsolete(&hash));
    }

    use garde::Validate;

    #[test]
    fn password_accepts_valid() {
        let p = Password("hunter22".to_string());
        assert!(p.validate().is_ok());
    }

    #[test]
    fn password_rejects_short() {
        let p = Password("short".to_string());
        let Err(report) = p.validate() else {
            panic!("expected error");
        };
        for (_path, err) in report.iter() {
            assert!(err.message().contains("length"));
            assert!(!err.message().contains("ascii"));
            assert!(!err.message().contains("whitespace"));
            assert!(!err.message().contains("spaces"));
        }
    }

    #[test]
    fn password_rejects_non_ascii() {
        let p = Password("hunter😀22".to_string());
        let Err(report) = p.validate() else {
            panic!("expected error");
        };
        for (_, err) in report.iter() {
            assert_eq!(err.message(), "not ascii");
        }
    }

    #[test]
    fn password_rejects_whitespace() {
        let p = Password("hunter 22".to_string());
        let Err(report) = p.validate() else {
            panic!("expected error");
        };
        for (_, err) in report.iter() {
            assert_eq!(err.message(), "must not contain spaces");
        }
    }

    #[test]
    fn password_accumulates_multiple_errors() {
        let p = Password("sh ort".to_string()); // 5 chars + whitespace
        let Err(report) = p.validate() else {
            panic!("expected error");
        };
        let mut messages: Vec<&str> = report.iter().map(|(_, e)| e.message()).collect();
        messages.sort();
        assert_eq!(messages.len(), 2);
        assert!(messages.contains(&"length is lower than 8"));
        assert!(messages.contains(&"must not contain spaces"));
    }
}
