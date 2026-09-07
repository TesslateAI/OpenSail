//! Deployment desired spec accepted from Control.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::reconcile::deployment::DeploymentDesired;
use crate::specs::hex_sha;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeploymentDesiredName {
    Running,
    Stopped,
    Absent,
}

impl DeploymentDesiredName {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "stopped" => Some(Self::Stopped),
            "absent" => Some(Self::Absent),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Absent => "absent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentSpec {
    pub revision: i64,
    pub desired: DeploymentDesiredName,
    pub release_id: Uuid,
    pub release_hash: String,
    #[serde(default)]
    pub runtime_profile: String,
    pub slug: String,
    pub kind: String,
    pub port: u16,
    pub run_argv: Vec<String>,
    #[serde(default)]
    pub health_path: String,
    #[serde(default)]
    pub cpu_millis: u32,
    #[serde(default)]
    pub memory_mb: u32,
    /// Predecessor Deployment. Activate loads this from the stored spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_deployment_id: Option<Uuid>,
    /// Restart generation. ReplacePod until the live Pod label matches.
    #[serde(default)]
    pub pod_generation: i64,
}

impl DeploymentSpec {
    pub fn planner_desired(&self) -> DeploymentDesired {
        match self.desired {
            DeploymentDesiredName::Running => DeploymentDesired::Running,
            DeploymentDesiredName::Stopped => DeploymentDesired::Stopped,
            DeploymentDesiredName::Absent => DeploymentDesired::Absent,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.revision < 1 {
            return Err("revision must be >= 1");
        }
        if self.desired == DeploymentDesiredName::Running {
            if self.release_id.is_nil() {
                return Err("releaseId must be a UUID");
            }
            if self.release_hash.is_empty() {
                return Err("releaseHash is required");
            }
            if self.slug.is_empty() {
                return Err("slug is required");
            }
            if self.kind != "dev" && self.kind != "prod" {
                return Err("kind must be dev or prod");
            }
            if self.port == 0 {
                return Err("port must be a non-zero HTTP port");
            }
            if self.run_argv.is_empty() {
                return Err("runArgv is required");
            }
            if self.runtime_profile.is_empty() {
                return Err("runtimeProfile is required");
            }
            if self.health_path.is_empty() {
                return Err("healthPath is required");
            }
            if self.cpu_millis == 0 || self.memory_mb == 0 {
                return Err("cpuMillis and memoryMb are required");
            }
        }
        Ok(())
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
        let spec: DeploymentSpec = serde_json::from_str(
            r#"{
                "revision": 15,
                "desired": "running",
                "releaseId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "releaseHash": "abc",
                "runtimeProfile": "universal-v1",
                "slug": "invoice-demo",
                "kind": "dev",
                "port": 3000,
                "runArgv": ["node", "dist/server.js"],
                "healthPath": "/healthz",
                "cpuMillis": 500,
                "memoryMb": 512
            }"#,
        )
        .expect("parses");
        assert_eq!(spec.revision, 15);
        assert_eq!(spec.desired, DeploymentDesiredName::Running);
        assert_eq!(
            spec.run_argv,
            vec!["node".to_owned(), "dist/server.js".into()]
        );
        assert_eq!(spec.planner_desired(), DeploymentDesired::Running);
        spec.validate().expect("running spec is complete");
    }

    #[test]
    fn refuses_unknown_fields_and_incomplete_running_spec() {
        assert!(
            serde_json::from_str::<DeploymentSpec>(
                r#"{
                    "revision": 1,
                    "desired": "running",
                    "releaseId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                    "releaseHash": "abc",
                    "slug": "app",
                    "kind": "dev",
                    "port": 3000,
                    "runArgv": ["true"],
                    "image": "evil"
                }"#
            )
            .is_err()
        );
        let incomplete: DeploymentSpec = serde_json::from_str(
            r#"{
                "revision": 1,
                "desired": "running",
                "releaseId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "releaseHash": "abc",
                "slug": "app",
                "kind": "dev",
                "port": 3000,
                "runArgv": ["true"]
            }"#,
        )
        .expect("missing running fields still decode as empty");
        assert_eq!(incomplete.validate(), Err("runtimeProfile is required"));
        let zero_port: DeploymentSpec = serde_json::from_str(
            r#"{
                "revision": 1,
                "desired": "running",
                "releaseId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "releaseHash": "abc",
                "runtimeProfile": "universal-v1",
                "slug": "app",
                "kind": "dev",
                "port": 0,
                "runArgv": ["true"],
                "healthPath": "/healthz",
                "cpuMillis": 500,
                "memoryMb": 512
            }"#,
        )
        .expect("port 0 still decodes as u16");
        assert_eq!(
            zero_port.validate(),
            Err("port must be a non-zero HTTP port")
        );
        assert!(
            serde_json::from_str::<DeploymentSpec>(
                r#"{
                    "revision": 1,
                    "desired": "running",
                    "releaseId": "",
                    "releaseHash": "abc",
                    "slug": "app",
                    "kind": "dev",
                    "port": 3000,
                    "runArgv": ["true"]
                }"#
            )
            .is_err()
        );
    }
}
