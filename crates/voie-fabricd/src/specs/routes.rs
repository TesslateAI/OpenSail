//! Derived route-map spec. Control sends one revision; Fabric applies atomically.

use serde::{Deserialize, Serialize};

use crate::specs::hex_sha;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteEntry {
    pub slug: String,
    pub kind: String,
    pub service: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteMapSpec {
    pub revision: i64,
    #[serde(default)]
    pub console_host: String,
    #[serde(default)]
    pub routes: Vec<RouteEntry>,
}

impl RouteMapSpec {
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
        let spec: RouteMapSpec = serde_json::from_str(
            r#"{
                "revision": 41,
                "consoleHost": "example.test",
                "routes": [
                    {"slug": "invoice-demo", "kind": "dev", "service": "10.43.0.10:3000"}
                ]
            }"#,
        )
        .expect("parses");
        assert_eq!(spec.revision, 41);
        assert_eq!(spec.routes.len(), 1);
        assert_eq!(spec.routes[0].slug, "invoice-demo");
    }
}
