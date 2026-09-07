//! Globally unique Application slug contract. ApplicationStore allocates it.

use uuid::Uuid;

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

/// `slug_base` plus `-` plus 8 hex characters.
const SUFFIX_LEN: usize = 8;
const MAX_BASE: usize = 39;

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

/// Display-name slug stem. Never used as the persisted identity by itself.
pub fn slug_base(name: &str) -> String {
    let mut out = String::new();
    let mut pending_hyphen = false;
    for ch in name.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            if pending_hyphen && !out.is_empty() {
                out.push('-');
            }
            pending_hyphen = false;
            out.push(c);
        } else if !out.is_empty() {
            pending_hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() < 3 {
        out = if out.is_empty() {
            "app".into()
        } else {
            format!("app-{out}")
        };
    }
    if out.len() > MAX_BASE {
        out.truncate(MAX_BASE);
        while out.ends_with('-') {
            out.pop();
        }
        if out.len() < 3 {
            out = "app".into();
        }
    }
    out
}

/// Server-owned unique slug: `{slug_base}-{8 hex}`.
pub fn allocate(name: &str) -> String {
    let base = slug_base(name);
    let suffix = format!("{:08x}", (Uuid::new_v4().as_u128() & 0xffff_ffff) as u32);
    let slug = format!("{base}-{}", &suffix[..SUFFIX_LEN]);
    debug_assert!(validate(&slug).is_ok(), "{slug}");
    slug
}

fn is_alnum(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'0'..=b'9')
}

pub fn reserved_names() -> &'static [&'static str] {
    RESERVED
}

#[cfg(test)]
mod tests {
    use super::{allocate, slug_base, validate};

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

    #[test]
    fn allocate_is_unique_and_valid() {
        assert_eq!(slug_base("Todo List App"), "todo-list-app");
        let a = allocate("Todo List App");
        let b = allocate("Todo List App");
        assert_ne!(a, b);
        assert!(a.starts_with("todo-list-app-"), "{a}");
        assert!(validate(&a).is_ok(), "{a}");
        assert!(validate(&allocate("A")).is_ok());
        assert!(validate(&allocate("!!!")).is_ok());
    }
}
