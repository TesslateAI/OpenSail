//! Typed desired specs persisted in Fabric SQLite before local effects.
//! Rendered Kubernetes YAML is not recovery truth.

pub mod accept;
pub mod database;
pub mod deployment;
pub mod routes;
pub mod traffic;
pub mod workspace;

pub fn hex_sha(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub(crate) fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}
