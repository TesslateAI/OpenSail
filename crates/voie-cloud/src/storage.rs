//! Platform storage tiers. Exact sizes are Fabric-allocated; this module
//! records the product contract and PostgreSQL defaults.

pub const GIB: i64 = 1024 * 1024 * 1024;

pub const WORKSPACE_BYTES: i64 = 16 * GIB;
pub const WORKSPACE_LARGE_BYTES: i64 = 32 * GIB;
pub const WORKSPACE_ELEVATED_BYTES: i64 = 64 * GIB;
pub const DATABASE_DEV_BYTES: i64 = 8 * GIB;
pub const DATABASE_DEV_ELEVATED_BYTES: i64 = 16 * GIB;
pub const DATABASE_PROD_BYTES: i64 = 16 * GIB;
pub const DATABASE_PROD_ELEVATED_BYTES: i64 = 32 * GIB;
pub const DEPLOYMENT_BYTES: i64 = GIB;

pub const BACKUP_RETENTION: i64 = 14;
/// Unpinned backup/snapshot Blob bytes retained per Database or Workspace.
/// Count retention alone cannot stop unbounded object size.
pub const BACKUP_BYTE_BUDGET: i64 = 32 * GIB;
pub const MAX_INFLIGHT_BACKUPS_PER_DATABASE: i64 = 1;
pub const MAX_INFLIGHT_BACKUPS_PER_PROJECT: i64 = 1;

/// Newest-first items: keep until count or byte budget is exhausted.
/// Always keep the newest object even when it alone exceeds the byte budget.
/// Once one object is expired, every older object expires with it.
pub fn expired_by_retention<T>(
    newest_first: impl IntoIterator<Item = T>,
    byte_length: impl Fn(&T) -> i64,
) -> Vec<T> {
    let mut kept = 0i64;
    let mut sum = 0i64;
    let mut expiring = false;
    let mut expired = Vec::new();
    for item in newest_first {
        if !expiring {
            let size = byte_length(&item).max(0);
            let over_count = kept >= BACKUP_RETENTION;
            let over_bytes = kept > 0 && sum.saturating_add(size) > BACKUP_BYTE_BUDGET;
            if !(over_count || over_bytes) {
                kept += 1;
                sum = sum.saturating_add(size);
                continue;
            }
            expiring = true;
        }
        expired.push(item);
    }
    expired
}

pub fn database_bytes(prod: bool, elevated: bool) -> i64 {
    match (prod, elevated) {
        (false, false) => DATABASE_DEV_BYTES,
        (false, true) => DATABASE_DEV_ELEVATED_BYTES,
        (true, false) => DATABASE_PROD_BYTES,
        (true, true) => DATABASE_PROD_ELEVATED_BYTES,
    }
}

/// Create-time size is always the default 16 GiB virtual disk.
/// Elevated (64 GiB) is a later 32→64 grow after `increase_resource_tier`.
pub fn workspace_bytes(_elevated: bool) -> i64 {
    WORKSPACE_BYTES
}

pub fn workspace_bytes_for_tier(tier: &str) -> i64 {
    match tier {
        "elevated" => WORKSPACE_ELEVATED_BYTES,
        "large" => WORKSPACE_LARGE_BYTES,
        _ => WORKSPACE_BYTES,
    }
}

pub fn workspace_tier_for_bytes(bytes: i64) -> &'static str {
    if bytes == WORKSPACE_ELEVATED_BYTES {
        "elevated"
    } else if bytes == WORKSPACE_LARGE_BYTES {
        "large"
    } else {
        "default"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_application_is_forty_two_gib() {
        assert_eq!(
            WORKSPACE_BYTES
                + DATABASE_DEV_BYTES
                + DATABASE_PROD_BYTES
                + DEPLOYMENT_BYTES
                + DEPLOYMENT_BYTES,
            42 * GIB
        );
    }

    #[test]
    fn workspace_default_is_sixteen_gib() {
        assert_eq!(WORKSPACE_BYTES, 16 * GIB);
        assert_eq!(workspace_bytes(false), 16 * GIB);
        assert_eq!(workspace_bytes(true), 16 * GIB);
        assert_eq!(workspace_bytes_for_tier("large"), 32 * GIB);
        assert_eq!(workspace_bytes_for_tier("elevated"), 64 * GIB);
    }

    #[test]
    fn byte_budget_expires_older_objects_and_keeps_newest() {
        let items = vec![(1, 20 * GIB), (2, 20 * GIB), (3, 8 * GIB)];
        let expired = expired_by_retention(items, |item| item.1);
        assert_eq!(expired, vec![(2, 20 * GIB), (3, 8 * GIB)]);
    }

    #[test]
    fn newest_over_budget_is_still_kept() {
        let items = vec![(1, 40 * GIB), (2, GIB)];
        let expired = expired_by_retention(items, |item| item.1);
        assert_eq!(expired, vec![(2, GIB)]);
    }
}
