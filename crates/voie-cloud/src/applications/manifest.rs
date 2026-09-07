//! Typed ManifestV1 (`voie.toml`). Unknown keys are errors.

use serde::Deserialize;
use sha2::{Digest, Sha256};

const ALLOWED_RUNTIMES: &[&str] = &["universal-v1"];
pub const MAX_CPU_MILLIS: u32 = 2000;
pub const MAX_MEMORY_MB: u32 = 2048;
pub const MIN_CPU_MILLIS: u32 = 100;
pub const MIN_MEMORY_MB: u32 = 128;
pub const DEFAULT_CPU_MILLIS: u32 = 500;
pub const DEFAULT_MEMORY_MB: u32 = 512;

/// Validated Application manifest. `Manifest` is the stable product alias.
pub type Manifest = ManifestV1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestV1 {
    pub version: u32,
    pub runtime: String,
    pub build_command: Vec<String>,
    pub build_output: String,
    pub test_command: Option<Vec<String>>,
    pub run_command: Vec<String>,
    pub run_port: u16,
    pub health_path: String,
    pub postgres: bool,
    pub migration_command: Option<Vec<String>>,
    pub cpu_millis: u32,
    pub memory_mb: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestV1File {
    version: u32,
    application: ApplicationFile,
    build: BuildFile,
    #[serde(default)]
    test: Option<TestFile>,
    run: RunFile,
    #[serde(default)]
    database: Option<DatabaseFile>,
    #[serde(default)]
    resources: Option<ResourcesFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplicationFile {
    runtime: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildFile {
    command: Vec<String>,
    output: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestFile {
    command: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunFile {
    command: Vec<String>,
    #[serde(default = "default_http_port")]
    port: u16,
    #[serde(default)]
    health_path: Option<String>,
}

fn default_http_port() -> u16 {
    8080
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DatabaseFile {
    #[serde(default)]
    postgres: bool,
    #[serde(default)]
    migration_command: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourcesFile {
    cpu_millis: u32,
    memory_mb: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    Empty,
    Decode(String),
    Field { field: String, message: String },
}

impl ManifestError {
    pub fn message(&self) -> String {
        match self {
            ManifestError::Empty => "application manifest is empty".into(),
            ManifestError::Decode(message) => message.clone(),
            ManifestError::Field { field, message } => format!("{field}: {message}"),
        }
    }
}

impl ManifestV1 {
    pub fn parse(bytes: &str) -> Result<Self, ManifestError> {
        let text = bytes.trim();
        if text.is_empty() {
            return Err(ManifestError::Empty);
        }
        let file: ManifestV1File =
            toml::from_str(text).map_err(|error| ManifestError::Decode(error.to_string()))?;
        file.into_manifest()
    }

    /// JSON Schema for the guest `voie.toml`. Supplied on product tool
    /// parameters as `$defs.ManifestV1`.
    pub fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["version", "application", "build", "run"],
            "properties": {
                "version": { "const": 1 },
                "application": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["runtime"],
                    "properties": {
                        "runtime": { "const": "universal-v1" }
                    }
                },
                "build": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["command", "output"],
                    "properties": {
                        "command": {
                            "type": "array",
                            "minItems": 1,
                            "items": { "type": "string", "minLength": 1 }
                        },
                        "output": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Relative directory of packed files, such as dist or . Never an absolute path."
                        }
                    }
                },
                "test": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["command"],
                    "properties": {
                        "command": {
                            "type": "array",
                            "minItems": 1,
                            "items": { "type": "string", "minLength": 1 }
                        }
                    }
                },
                "run": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["command"],
                    "properties": {
                        "command": {
                            "type": "array",
                            "minItems": 1,
                            "items": { "type": "string", "minLength": 1 }
                        },
                        "port": { "type": "integer", "minimum": 1, "maximum": 65535 },
                        "health_path": { "type": "string", "pattern": "^/" }
                    }
                },
                "database": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "postgres": { "type": "boolean" },
                        "migration_command": {
                            "type": "array",
                            "minItems": 1,
                            "items": { "type": "string", "minLength": 1 }
                        }
                    }
                },
                "resources": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["cpu_millis", "memory_mb"],
                    "properties": {
                        "cpu_millis": {
                            "type": "integer",
                            "minimum": MIN_CPU_MILLIS,
                            "maximum": MAX_CPU_MILLIS
                        },
                        "memory_mb": {
                            "type": "integer",
                            "minimum": MIN_MEMORY_MB,
                            "maximum": MAX_MEMORY_MB
                        }
                    }
                }
            }
        })
    }

    /// Default Platform tier. Larger `voie.toml` resources require the
    /// `increase_resource_tier` approval before a Release is reserved.
    pub fn exceeds_default_tier(&self) -> bool {
        self.cpu_millis > DEFAULT_CPU_MILLIS || self.memory_mb > DEFAULT_MEMORY_MB
    }

    pub fn hash(&self, original: &str) -> [u8; 32] {
        Sha256::digest(original.as_bytes()).into()
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "version": self.version,
            "runtime": self.runtime,
            "build": { "command": self.build_command, "output": self.build_output },
            "test": self.test_command.as_ref().map(|command| serde_json::json!({ "command": command })),
            "run": {
                "command": self.run_command,
                "port": self.run_port,
                "healthPath": self.health_path,
            },
            "database": {
                "postgres": self.postgres,
                "migrationCommand": self.migration_command,
            },
            "resources": {
                "cpuMillis": self.cpu_millis,
                "memoryMb": self.memory_mb,
            },
        })
    }
}

impl ManifestV1File {
    fn into_manifest(self) -> Result<ManifestV1, ManifestError> {
        if self.version != 1 {
            return Err(field("version", "must be 1"));
        }
        if !ALLOWED_RUNTIMES.contains(&self.application.runtime.as_str()) {
            return Err(field("application.runtime", "is not a platform profile"));
        }
        argv("build.command", &self.build.command)?;
        relative_path("build.output", &self.build.output)?;
        let test_command = match self.test {
            Some(test) => {
                argv("test.command", &test.command)?;
                Some(test.command)
            }
            None => None,
        };
        argv("run.command", &self.run.command)?;
        if self.run.port == 0 {
            return Err(field("run.port", "must be a single HTTP port"));
        }
        let health_path = match self.run.health_path {
            Some(path) => {
                if !path.starts_with('/') || path.contains("..") {
                    return Err(field(
                        "run.health_path",
                        "must be an absolute guest path without ..",
                    ));
                }
                path
            }
            None => "/healthz".to_owned(),
        };
        let (postgres, migration_command) = match self.database {
            Some(database) => {
                let migration = match database.migration_command {
                    Some(command) => {
                        argv("database.migration_command", &command)?;
                        Some(command)
                    }
                    None => None,
                };
                (database.postgres, migration)
            }
            None => (false, None),
        };
        let (cpu_millis, memory_mb) = match self.resources {
            Some(resources) => (
                in_range(
                    "resources.cpu_millis",
                    resources.cpu_millis,
                    MIN_CPU_MILLIS,
                    MAX_CPU_MILLIS,
                )?,
                in_range(
                    "resources.memory_mb",
                    resources.memory_mb,
                    MIN_MEMORY_MB,
                    MAX_MEMORY_MB,
                )?,
            ),
            None => (DEFAULT_CPU_MILLIS, DEFAULT_MEMORY_MB),
        };
        Ok(ManifestV1 {
            version: 1,
            runtime: self.application.runtime,
            build_command: self.build.command,
            build_output: self.build.output,
            test_command,
            run_command: self.run.command,
            run_port: self.run.port,
            health_path,
            postgres,
            migration_command,
            cpu_millis,
            memory_mb,
        })
    }
}

fn field(field: &str, message: &str) -> ManifestError {
    ManifestError::Field {
        field: field.to_owned(),
        message: message.to_owned(),
    }
}

fn argv(path: &str, command: &[String]) -> Result<(), ManifestError> {
    if command.is_empty() {
        return Err(field(path, "must be a non-empty argv vector"));
    }
    for part in command {
        if part.is_empty() || part.contains('\0') {
            return Err(field(path, "must be a non-empty argv vector"));
        }
    }
    Ok(())
}

fn in_range(path: &str, value: u32, min: u32, max: u32) -> Result<u32, ManifestError> {
    if value < min || value > max {
        return Err(field(path, "is outside platform limits"));
    }
    Ok(value)
}

fn relative_path(path: &str, value: &str) -> Result<(), ManifestError> {
    if value.is_empty() || value.starts_with('/') || value.contains('\0') {
        return Err(field(path, "must be relative and stay inside the root"));
    }
    for component in value.split('/') {
        if component == ".." || component.is_empty() {
            return Err(field(path, "must be relative and stay inside the root"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ManifestV1;

    const SAMPLE: &str = r#"
version = 1
[application]
runtime = "universal-v1"
[build]
command = ["sh", ".voie/build.sh"]
output = "dist"
[test]
command = ["sh", ".voie/test.sh"]
[run]
command = ["node", "dist/server.js"]
port = 3000
health_path = "/healthz"
[database]
postgres = true
migration_command = ["node", "dist/migrate.js"]
[resources]
cpu_millis = 500
memory_mb = 512
"#;

    #[test]
    fn parses_supported_manifest() {
        let manifest = ManifestV1::parse(SAMPLE).expect("sample parses");
        assert_eq!(manifest.runtime, "universal-v1");
        assert_eq!(manifest.run_port, 3000);
        assert!(manifest.postgres);
        assert!(!manifest.exceeds_default_tier());
    }

    #[test]
    fn default_resources_are_the_platform_tier() {
        let text = r#"
version = 1
[application]
runtime = "universal-v1"
[build]
command = ["true"]
output = "."
[run]
command = ["true"]
port = 3000
"#;
        let manifest = ManifestV1::parse(text).expect("parses");
        assert_eq!(manifest.cpu_millis, super::DEFAULT_CPU_MILLIS);
        assert_eq!(manifest.memory_mb, super::DEFAULT_MEMORY_MB);
        assert!(!manifest.exceeds_default_tier());
        let high = format!("{text}\n[resources]\ncpu_millis = 2000\nmemory_mb = 2048\n");
        let raised = ManifestV1::parse(&high).expect("raised parses");
        assert!(raised.exceeds_default_tier());
        let over = format!("{text}\n[resources]\ncpu_millis = 2001\nmemory_mb = 512\n");
        assert!(ManifestV1::parse(&over).is_err());
    }

    #[test]
    fn rejects_infrastructure_fields() {
        let text = format!("{SAMPLE}\nimage = \"evil:latest\"\n");
        let unknown = ManifestV1::parse(&text).expect_err("unknown top-level key");
        assert!(
            unknown.message().contains("image"),
            "field-level error must name image: {}",
            unknown.message()
        );
        assert!(ManifestV1::parse("version = 1\nkubernetes = true\n").is_err());
        assert!(
            ManifestV1::parse(&format!("{SAMPLE}\nstorage_bytes = 34359738368\n")).is_err(),
            "storage tiers are selected by VOIE, not voie.toml"
        );
        assert!(ManifestV1::parse(&format!("{SAMPLE}\nstorage_tier = \"elevated\"\n")).is_err());
        let with_word = r#"
version = 1
[application]
runtime = "universal-v1"
[build]
command = ["echo", "container"]
output = "dist"
[run]
command = ["true"]
port = 3000
"#;
        ManifestV1::parse(with_word).expect("values may mention infrastructure words");
    }

    #[test]
    fn omitted_port_defaults_to_8080() {
        let missing_port = r#"
version = 1
[application]
runtime = "universal-v1"
[build]
command = ["true"]
output = "."
[run]
command = ["true"]
"#;
        let manifest = ManifestV1::parse(missing_port).expect("port defaults");
        assert_eq!(manifest.run_port, 8080);
        let zero = format!("{SAMPLE}\n");
        let zero = zero.replace("port = 3000", "port = 0");
        let err = ManifestV1::parse(&zero).expect_err("port 0");
        assert!(
            err.message().contains("run.port"),
            "zero port must name run.port: {}",
            err.message()
        );
    }

    #[test]
    fn json_schema_denies_unknown_fields() {
        let schema = ManifestV1::json_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["required"],
            serde_json::json!(["version", "application", "build", "run"])
        );
        assert_eq!(
            schema["properties"]["run"]["properties"]["port"]["minimum"],
            1
        );
    }
}
