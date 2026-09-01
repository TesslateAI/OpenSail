//! Globally unique Application slug contract.

const RESERVED: &[&str] = &[
    "admin",
    "api",
    "auth",
    "console",
    "dev",
    "prod",
    "hs",
    "headscale",
    "support",
    "status",
    "www",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlugError {
    Empty,
    Length,
    Charset,
    Reserved,
}

impl SlugError {
    pub fn message(self) -> &'static str {
        match self {
            SlugError::Empty => "application slug is empty",
            SlugError::Length => "application slug must be 3 to 48 characters",
            SlugError::Charset => {
                "application slug must be lowercase ASCII letters, digits, and hyphen"
            }
            SlugError::Reserved => "application slug is reserved",
        }
    }
}

/// Validates a globally unique Application slug.
///
/// Rules: 3..=48 lowercase ASCII letters, digits, hyphen; must start and end
/// with a letter or digit; no consecutive hyphens; not a reserved name.
pub fn validate(slug: &str) -> Result<(), SlugError> {
    let trimmed = slug.trim();
    if trimmed.is_empty() {
        return Err(SlugError::Empty);
    }
    if trimmed.len() < 3 || trimmed.len() > 48 {
        return Err(SlugError::Length);
    }
    let bytes = trimmed.as_bytes();
    if !is_alnum(bytes[0]) || !is_alnum(bytes[bytes.len() - 1]) {
        return Err(SlugError::Charset);
    }
    let mut previous_hyphen = false;
    for &byte in bytes {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' => previous_hyphen = false,
            b'-' if previous_hyphen => return Err(SlugError::Charset),
            b'-' => previous_hyphen = true,
            _ => return Err(SlugError::Charset),
        }
    }
    if RESERVED.contains(&trimmed) {
        return Err(SlugError::Reserved);
    }
    Ok(())
}

fn is_alnum(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'0'..=b'9')
}

pub fn reserved_names() -> &'static [&'static str] {
    RESERVED
}

#[cfg(test)]
mod tests {
    use super::validate;

    #[test]
    fn accepts_documented_examples() {
        for slug in ["invoice-demo", "acme-portal", "app7"] {
            assert!(validate(slug).is_ok(), "{slug}");
        }
    }

    #[test]
    fn rejects_documented_invalid_and_reserved() {
        for slug in [
            "Invoice",
            "-portal",
            "portal-",
            "portal.example.com",
            "admin",
            "a",
            "",
        ] {
            assert!(validate(slug).is_err(), "{slug}");
        }
    }
}
