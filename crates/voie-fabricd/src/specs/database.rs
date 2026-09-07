//! Database desired spec accepted from Control.

use serde::{Deserialize, Serialize};

use crate::reconcile::database::DatabaseDesired;
use crate::specs::hex_sha;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DatabaseDesiredName {
    Present,
    Suspended,
    Absent,
}

impl DatabaseDesiredName {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "present" => Some(Self::Present),
            "suspended" => Some(Self::Suspended),
            "absent" => Some(Self::Absent),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Suspended => "suspended",
            Self::Absent => "absent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DatabaseSpec {
    pub revision: i64,
    pub desired: DatabaseDesiredName,
    #[serde(default)]
    pub runtime_profile: String,
    #[serde(default)]
    pub security_profile: u32,
    /// Control names the platform tier. Fabric maps it to local bytes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub storage_tier: String,
    /// Legacy persisted specs and Fabric-local allocation size.
    #[serde(default, skip_serializing_if = "crate::specs::is_zero_u64")]
    pub volume_bytes: u64,
    #[serde(default)]
    pub credential_version: i64,
    pub slug: String,
    pub kind: String,
}

impl DatabaseSpec {
    pub fn planner_desired(&self) -> DatabaseDesired {
        match self.desired {
            DatabaseDesiredName::Present => DatabaseDesired::Present {
                security_profile: self.security_profile,
            },
            DatabaseDesiredName::Suspended => DatabaseDesired::Suspended,
            DatabaseDesiredName::Absent => DatabaseDesired::Absent,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.revision < 1 {
            return Err("revision must be >= 1");
        }
        if self.desired == DatabaseDesiredName::Present {
            if self.runtime_profile.is_empty() {
                return Err("runtimeProfile is required");
            }
            if self.security_profile < 1 {
                return Err("securityProfile is required");
            }
            if self.slug.is_empty() {
                return Err("slug is required");
            }
            if self.kind != "dev" && self.kind != "prod" {
                return Err("kind must be dev or prod");
            }
            let named = matches!(self.storage_tier.as_str(), "default" | "elevated");
            if !self.storage_tier.is_empty() && !named {
                return Err("storageTier must be default or elevated");
            }
            if !named && self.volume_bytes == 0 {
                return Err("storageTier or volumeBytes is required");
            }
        }
        Ok(())
    }

    /// Control `storageTier` wins. Byte-sized leftover specs still realize.
    pub fn volume_bytes_for(&self, policy: &crate::storage::StoragePolicy) -> u64 {
        match self.storage_tier.as_str() {
            "default" => policy.database_size(self.kind == "prod", false),
            "elevated" => policy.database_size(self.kind == "prod", true),
            _ => self.volume_bytes,
        }
    }

    pub fn hash_bytes(&self) -> String {
        use sha2::{Digest, Sha256};
        let body = serde_json::to_vec(self).unwrap_or_default();
        hex_sha(&Sha256::digest(&body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_control_json() {
        let spec: DatabaseSpec = serde_json::from_str(
            r#"{
                "revision": 8,
                "desired": "present",
                "runtimeProfile": "voie-postgres:v1",
                "securityProfile": 2,
                "storageTier": "elevated",
                "credentialVersion": 4,
                "slug": "invoice-demo",
                "kind": "prod"
            }"#,
        )
        .expect("parses");
        assert_eq!(spec.revision, 8);
        assert_eq!(spec.desired, DatabaseDesiredName::Present);
        assert_eq!(spec.security_profile, 2);
        assert_eq!(spec.storage_tier, "elevated");
        assert_eq!(
            spec.planner_desired(),
            DatabaseDesired::Present {
                security_profile: 2
            }
        );
        spec.validate().expect("present spec is complete");
        let incomplete: DatabaseSpec = serde_json::from_str(
            r#"{
                "revision": 8,
                "desired": "present",
                "volumeBytes": 17179869184,
                "slug": "invoice-demo",
                "kind": "prod"
            }"#,
        )
        .expect("legacy volumeBytes still loads");
        assert_eq!(incomplete.volume_bytes, 17179869184);
        assert!(incomplete.storage_tier.is_empty());
        assert_eq!(
            incomplete.validate(),
            Err("runtimeProfile is required"),
            "missing runtimeProfile is not invented"
        );
        let legacy: DatabaseSpec = serde_json::from_str(
            r#"{
                "revision": 8,
                "desired": "present",
                "runtimeProfile": "voie-postgres:v1",
                "securityProfile": 1,
                "volumeBytes": 17179869184,
                "slug": "invoice-demo",
                "kind": "prod"
            }"#,
        )
        .expect("legacy volumeBytes with required fields still loads");
        assert_eq!(legacy.volume_bytes, 17179869184);
        legacy.validate().expect("legacy present spec is complete");
    }

    #[test]
    fn refuses_unknown_fields() {
        assert!(
            serde_json::from_str::<DatabaseSpec>(
                r#"{
                    "revision": 1,
                    "desired": "present",
                    "storageTier": "default",
                    "slug": "app",
                    "kind": "dev",
                    "hostPath": "/evil"
                }"#
            )
            .is_err()
        );
    }
}
