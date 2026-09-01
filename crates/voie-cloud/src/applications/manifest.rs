//! Small declarative Application manifest (`voie.toml`).

use sha2::{Digest, Sha256};

const FORBIDDEN: &[&str] = &[
    "image",
    "container",
    "kubernetes",
    "k8s",
    "host_path",
    "hostpath",
    "privileged",
    "service_account",
    "serviceaccount",
    "network_namespace",
    "volume_device",
    "fabric",
    "ingress",
    "storage_bytes",
    "storage_tier",
    "disk_size",
    "volume_size",
    "lv_size",
    "workspace_size",
];

const ALLOWED_RUNTIMES: &[&str] = &["universal-v1"];
pub const MAX_CPU_MILLIS: u32 = 2000;
pub const MAX_MEMORY_MB: u32 = 2048;
pub const MIN_CPU_MILLIS: u32 = 100;
pub const MIN_MEMORY_MB: u32 = 128;
pub const DEFAULT_CPU_MILLIS: u32 = 500;
pub const DEFAULT_MEMORY_MB: u32 = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestError {
    Empty,
    Parse,
    Version,
    Runtime,
    ForbiddenField,
    Command,
    Path,
    Port,
    Resources,
}

impl ManifestError {
    pub fn message(self) -> &'static str {
        match self {
            ManifestError::Empty => "application manifest is empty",
            ManifestError::Parse => "application manifest is not valid TOML",
            ManifestError::Version => "application manifest version must be 1",
            ManifestError::Runtime => "application runtime is not a platform profile",
            ManifestError::ForbiddenField => "application manifest names infrastructure VOIE owns",
            ManifestError::Command => "application command must be a non-empty argv vector",
            ManifestError::Path => "application path must be relative and stay inside the root",
            ManifestError::Port => "application port must be a single HTTP port",
            ManifestError::Resources => "application resources are outside platform limits",
        }
    }
}

impl Manifest {
    pub fn parse(bytes: &str) -> Result<Self, ManifestError> {
        let text = bytes.trim();
        if text.is_empty() {
            return Err(ManifestError::Empty);
        }
        let lower = text.to_ascii_lowercase();
        for needle in FORBIDDEN {
            if lower.contains(needle) {
                return Err(ManifestError::ForbiddenField);
            }
        }
        let table: toml::Value = toml::from_str(text).map_err(|_| ManifestError::Parse)?;
        let version = table
            .get("version")
            .and_then(toml::Value::as_integer)
            .ok_or(ManifestError::Version)?;
        if version != 1 {
            return Err(ManifestError::Version);
        }
        let application = table.get("application").ok_or(ManifestError::Parse)?;
        let runtime = string_field(application, "runtime")?;
        if !ALLOWED_RUNTIMES.contains(&runtime.as_str()) {
            return Err(ManifestError::Runtime);
        }
        let build = table.get("build").ok_or(ManifestError::Parse)?;
        let build_command = argv(build, "command")?;
        let build_output = string_field(build, "output")?;
        relative_path(&build_output)?;
        let test_command = match table.get("test") {
            Some(test) => Some(argv(test, "command")?),
            None => None,
        };
        let run = table.get("run").ok_or(ManifestError::Parse)?;
        let run_command = argv(run, "command")?;
        let run_port = run
            .get("port")
            .and_then(toml::Value::as_integer)
            .ok_or(ManifestError::Port)?;
        if run_port < 1 || run_port > 65535 {
            return Err(ManifestError::Port);
        }
        let health_path =
            string_field(run, "health_path").unwrap_or_else(|_| "/healthz".to_owned());
        if !health_path.starts_with('/') || health_path.contains("..") {
            return Err(ManifestError::Path);
        }
        let (postgres, migration_command) = match table.get("database") {
            Some(database) => {
                let postgres = database
                    .get("postgres")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(false);
                let migration = match database.get("migration_command") {
                    Some(_) => Some(argv(database, "migration_command")?),
                    None => None,
                };
                (postgres, migration)
            }
            None => (false, None),
        };
        let (cpu_millis, memory_mb) = match table.get("resources") {
            Some(resources) => (
                integer_field(resources, "cpu_millis", MIN_CPU_MILLIS, MAX_CPU_MILLIS)?,
                integer_field(resources, "memory_mb", MIN_MEMORY_MB, MAX_MEMORY_MB)?,
            ),
            None => (DEFAULT_CPU_MILLIS, DEFAULT_MEMORY_MB),
        };
        Ok(Manifest {
            version: 1,
            runtime,
            build_command,
            build_output,
            test_command,
            run_command,
            run_port: run_port as u16,
            health_path,
            postgres,
            migration_command,
            cpu_millis,
            memory_mb,
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

fn string_field(value: &toml::Value, key: &str) -> Result<String, ManifestError> {
    value
        .get(key)
        .and_then(toml::Value::as_str)
        .map(|text| text.to_owned())
        .ok_or(ManifestError::Parse)
}

fn argv(value: &toml::Value, key: &str) -> Result<Vec<String>, ManifestError> {
    let array = value
        .get(key)
        .and_then(toml::Value::as_array)
        .ok_or(ManifestError::Command)?;
    if array.is_empty() {
        return Err(ManifestError::Command);
    }
    let mut command = Vec::new();
    for item in array {
        let part = item.as_str().ok_or(ManifestError::Command)?;
        if part.is_empty() || part.contains('\0') {
            return Err(ManifestError::Command);
        }
        command.push(part.to_owned());
    }
    Ok(command)
}

fn integer_field(value: &toml::Value, key: &str, min: u32, max: u32) -> Result<u32, ManifestError> {
    let number = value
        .get(key)
        .and_then(toml::Value::as_integer)
        .ok_or(ManifestError::Resources)?;
    if number < i64::from(min) || number > i64::from(max) {
        return Err(ManifestError::Resources);
    }
    Ok(number as u32)
}

fn relative_path(path: &str) -> Result<(), ManifestError> {
    if path.is_empty() || path.starts_with('/') || path.contains('\0') {
        return Err(ManifestError::Path);
    }
    for component in path.split('/') {
        if component == ".." || component.is_empty() {
            return Err(ManifestError::Path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Manifest;

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
        let manifest = Manifest::parse(SAMPLE).expect("sample parses");
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
        let manifest = Manifest::parse(text).expect("parses");
        assert_eq!(manifest.cpu_millis, super::DEFAULT_CPU_MILLIS);
        assert_eq!(manifest.memory_mb, super::DEFAULT_MEMORY_MB);
        assert!(!manifest.exceeds_default_tier());
        let high = format!("{text}\n[resources]\ncpu_millis = 2000\nmemory_mb = 2048\n");
        let raised = Manifest::parse(&high).expect("raised parses");
        assert!(raised.exceeds_default_tier());
        let over = format!("{text}\n[resources]\ncpu_millis = 2001\nmemory_mb = 512\n");
        assert!(Manifest::parse(&over).is_err());
    }

    #[test]
    fn rejects_infrastructure_fields() {
        let text = format!("{SAMPLE}\nimage = \"evil:latest\"\n");
        assert!(Manifest::parse(&text).is_err());
        assert!(Manifest::parse("version = 1\nkubernetes = true\n").is_err());
        assert!(
            Manifest::parse(&format!("{SAMPLE}\nstorage_bytes = 34359738368\n")).is_err(),
            "storage tiers are selected by VOIE, not voie.toml"
        );
        assert!(Manifest::parse(&format!("{SAMPLE}\nstorage_tier = \"elevated\"\n")).is_err());
    }
}
