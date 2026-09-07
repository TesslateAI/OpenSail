//! Workspace desired spec accepted from Control.

use serde::{Deserialize, Serialize};

use crate::reconcile::workspace::WorkspaceDesired;
use crate::specs::hex_sha;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceDesiredName {
    Active,
    Suspended,
    Archived,
    Deleted,
}

impl WorkspaceDesiredName {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "suspended" => Some(Self::Suspended),
            "archived" => Some(Self::Archived),
            "deleted" => Some(Self::Deleted),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Suspended => "suspended",
            Self::Archived => "archived",
            Self::Deleted => "deleted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceSpec {
    pub revision: i64,
    pub desired: WorkspaceDesiredName,
    #[serde(default)]
    pub runtime_profile: String,
    /// Control names the platform tier. Fabric maps it to local bytes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub storage_tier: String,
    /// Legacy persisted specs and Fabric-local allocation size.
    #[serde(default, skip_serializing_if = "crate::specs::is_zero_u64")]
    pub volume_bytes: u64,
}

impl WorkspaceSpec {
    pub fn planner_desired(&self) -> WorkspaceDesired {
        match self.desired {
            WorkspaceDesiredName::Active => WorkspaceDesired::Active,
            WorkspaceDesiredName::Suspended => WorkspaceDesired::Suspended,
            WorkspaceDesiredName::Archived | WorkspaceDesiredName::Deleted => {
                // Both drop the guest. Volume release stays on the
                // residue-gated delete path, not planner RemoveLv.
                WorkspaceDesired::Absent
            }
        }
    }

    pub fn hash_bytes(&self) -> String {
        use sha2::{Digest, Sha256};
        let body = serde_json::to_vec(self).unwrap_or_default();
        hex_sha(&Sha256::digest(&body))
    }

    /// Control `storageTier` wins. Byte-sized leftover specs still realize.
    pub fn volume_bytes_for(&self, policy: &crate::storage::StoragePolicy) -> u64 {
        match self.storage_tier.as_str() {
            "default" | "large" | "elevated" => policy.workspace_size_for_tier(&self.storage_tier),
            _ => self.volume_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_control_json() {
        let spec: WorkspaceSpec = serde_json::from_str(
            r#"{
                "revision": 3,
                "desired": "active",
                "runtimeProfile": "workspace-v1",
                "storageTier": "default"
            }"#,
        )
        .expect("parses");
        assert_eq!(spec.revision, 3);
        assert_eq!(spec.desired, WorkspaceDesiredName::Active);
        assert_eq!(spec.storage_tier, "default");
        assert_eq!(spec.planner_desired(), WorkspaceDesired::Active);
        let archived: WorkspaceSpec =
            serde_json::from_str(r#"{"revision":4,"desired":"archived"}"#).expect("parses");
        assert_eq!(archived.planner_desired(), WorkspaceDesired::Absent);
        let legacy: WorkspaceSpec = serde_json::from_str(
            r#"{"revision":3,"desired":"active","runtimeProfile":"workspace-v1","volumeBytes":17179869184}"#,
        )
        .expect("legacy volumeBytes still loads");
        assert_eq!(legacy.volume_bytes, 17179869184);
        assert!(legacy.storage_tier.is_empty());
    }

    #[test]
    fn refuses_unknown_fields() {
        assert!(
            serde_json::from_str::<WorkspaceSpec>(
                r#"{"revision":1,"desired":"active","storageTier":"default","image":"evil"}"#
            )
            .is_err()
        );
    }
}
