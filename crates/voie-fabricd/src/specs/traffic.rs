//! Environment traffic target accepted from Control.
//!
//! `desired_deployment_id = Some(id)` points the Environment Service at that
//! Deployment. `None` retires the selector and the Fabric gateway edge.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::specs::hex_sha;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficSpec {
    pub revision: i64,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub kind: String,
    /// Present UUID or explicit `null`. Omitted is invalid: `None` retires traffic.
    pub desired_deployment_id: Option<Uuid>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TrafficSpecDe {
    revision: i64,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    kind: String,
    desired_deployment_id: ExplicitOptionUuid,
}

/// Field must be present. JSON `null` is `None`; a UUID is `Some`.
struct ExplicitOptionUuid(Option<Uuid>);

impl<'de> Deserialize<'de> for ExplicitOptionUuid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(ExplicitOptionUuid(Option::<Uuid>::deserialize(
            deserializer,
        )?))
    }
}

impl<'de> Deserialize<'de> for TrafficSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let Some(object) = value.as_object() else {
            return Err(de::Error::custom("traffic spec must be an object"));
        };
        if !object.contains_key("desiredDeploymentId") {
            return Err(de::Error::missing_field("desiredDeploymentId"));
        }
        let raw = TrafficSpecDe::deserialize(value).map_err(de::Error::custom)?;
        Ok(TrafficSpec {
            revision: raw.revision,
            slug: raw.slug,
            kind: raw.kind,
            desired_deployment_id: raw.desired_deployment_id.0,
        })
    }
}

impl TrafficSpec {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.revision < 1 {
            return Err("revision must be >= 1");
        }
        if self.slug.is_empty() {
            return Err("slug is required");
        }
        if self.kind != "dev" && self.kind != "prod" {
            return Err("kind must be dev or prod");
        }
        if self.desired_deployment_id == Some(Uuid::nil()) {
            return Err("desiredDeploymentId must be a UUID");
        }
        Ok(())
    }

    pub fn matches_observed(&self, observed: Option<Uuid>) -> bool {
        self.desired_deployment_id == observed
    }

    pub fn observed_revision(&self, observed: Option<Uuid>) -> i64 {
        if self.matches_observed(observed) {
            self.revision
        } else {
            0
        }
    }

    pub fn observed_state(&self, observed: Option<Uuid>) -> &'static str {
        if !self.matches_observed(observed) {
            "pending"
        } else if self.desired_deployment_id.is_none() {
            "absent"
        } else {
            "active"
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
    fn round_trips_present_control_json() {
        let spec: TrafficSpec = serde_json::from_str(
            r#"{
                "revision": 4,
                "slug": "invoice-demo",
                "kind": "dev",
                "desiredDeploymentId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
            }"#,
        )
        .expect("parses");
        assert_eq!(spec.revision, 4);
        assert_eq!(spec.slug, "invoice-demo");
        assert_eq!(spec.kind, "dev");
        assert_eq!(
            spec.desired_deployment_id,
            Some("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".parse().unwrap())
        );
        spec.validate().expect("valid");
        let desired = spec.desired_deployment_id;
        assert!(spec.matches_observed(desired));
        assert!(!spec.matches_observed(None));
        assert_eq!(spec.observed_revision(desired), 4);
        assert_eq!(spec.observed_revision(None), 0);
        assert_eq!(spec.observed_state(desired), "active");
        assert_eq!(spec.observed_state(None), "pending");
    }

    #[test]
    fn round_trips_absent_control_json() {
        let spec: TrafficSpec = serde_json::from_str(
            r#"{
                "revision": 5,
                "slug": "invoice-demo",
                "kind": "prod",
                "desiredDeploymentId": null
            }"#,
        )
        .expect("parses");
        spec.validate().expect("valid");
        assert_eq!(spec.desired_deployment_id, None);
        assert!(spec.matches_observed(None));
        assert!(!spec.matches_observed(Some(
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".parse().unwrap()
        )));
        assert_eq!(spec.observed_state(None), "absent");
        assert_eq!(spec.observed_revision(None), 5);
    }

    #[test]
    fn omitted_desired_deployment_id_is_rejected() {
        assert!(
            serde_json::from_str::<TrafficSpec>(
                r#"{"revision":1,"slug":"invoice-demo","kind":"dev"}"#
            )
            .is_err(),
            "omitted desiredDeploymentId is not absent traffic"
        );
        assert!(serde_json::from_str::<TrafficSpec>("{}").is_err());
        let present: TrafficSpec = serde_json::from_str(
            r#"{"revision":1,"slug":"invoice-demo","kind":"dev","desiredDeploymentId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"}"#,
        )
        .expect("uuid field");
        assert!(present.desired_deployment_id.is_some());
        let absent: TrafficSpec = serde_json::from_str(
            r#"{"revision":1,"slug":"invoice-demo","kind":"dev","desiredDeploymentId":null}"#,
        )
        .expect("explicit null");
        assert_eq!(absent.desired_deployment_id, None);
    }

    #[test]
    fn refuses_unknown_fields() {
        assert!(serde_json::from_str::<TrafficSpec>(
            r#"{"revision":1,"slug":"invoice-demo","kind":"dev","desiredDeploymentId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","selector":"x"}"#
        )
        .is_err());
    }

    #[test]
    fn refuses_nil_target_and_bad_kind() {
        let nil: TrafficSpec = serde_json::from_str(
            r#"{"revision":1,"slug":"invoice-demo","kind":"dev","desiredDeploymentId":"00000000-0000-0000-0000-000000000000"}"#,
        )
        .expect("parses");
        assert_eq!(nil.validate(), Err("desiredDeploymentId must be a UUID"));
        let kind: TrafficSpec = serde_json::from_str(
            r#"{"revision":1,"slug":"invoice-demo","kind":"stage","desiredDeploymentId":null}"#,
        )
        .expect("parses");
        assert_eq!(kind.validate(), Err("kind must be dev or prod"));
    }
}
