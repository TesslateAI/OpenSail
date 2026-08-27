//! Durable local-encrypted material backend for single-node deployments.
//!
//! # Layout
//!
//! One file per secret reference under a configured directory:
//!
//! ```text
//! <VOIE_SECRETS_DIR or ./data/secrets>/<us-<uuid>>
//! ```
//!
//! Each file is format version 1:
//!
//! ```text
//! "voie-secret-vault-1" \0 <12-byte nonce> <AES-256-GCM ciphertext + 16-byte tag>
//! ```
//!
//! # Key material
//!
//! * `VOIE_SECRETS_KEY` — exactly 64 hex characters (32 bytes). Production
//!   deployments set this; the same key decrypts every stored secret.
//! * Without `VOIE_SECRETS_KEY`, the key is
//!   `SHA-256("voie-secret-vault" || database URL)`. This keeps development
//!   working with no extra configuration, stays stable across restarts on the
//!   same database, and never stores the key. Rotating the database URL makes
//!   previously written files undecryptable — harmless here because the
//!   control plane has no read path; operators rotate values through the API.
//!
//! # Permissions
//!
//! The storage directory is created `0700` and every secret file is written
//! `0600`, then atomically renamed into place. The backend never logs,
//! serializes, or returns material.

use std::path::{Path, PathBuf};

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::digest::SHA256_OUTPUT_LEN;
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};

use super::{
    BackendError, BackendFuture, BackendKind, BackendWrite, SecretBackend, SecretReference,
    SecretValue,
};

const FORMAT_TAG: &[u8] = b"voie-secret-vault-1";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
/// `VOIE_SECRETS_DIR`: material directory. Defaults to `./data/secrets`.
pub const SECRETS_DIR_ENV: &str = "VOIE_SECRETS_DIR";
/// `VOIE_SECRETS_KEY`: hex-encoded 32-byte key; falls back to the
/// database-URL-salt derivation when absent.
pub const SECRETS_KEY_ENV: &str = "VOIE_SECRETS_KEY";
pub const DEFAULT_SECRETS_DIR: &str = "./data/secrets";
const KEY_DERIVATION_SALT: &[u8] = b"voie-secret-vault";

/// Durable local material store. One encrypted, owner-only file per backend
/// reference inside an owner-only directory.
#[derive(Debug)]
pub struct FileSecretBackend {
    dir: PathBuf,
    key: [u8; KEY_LEN],
}

impl FileSecretBackend {
    /// Opens (creating if needed) the material directory and resolves the
    /// encryption key from the environment. `database_url` participates in
    /// the derived key only when no explicit key is configured.
    pub fn from_env(database_url: &str) -> Result<Self, String> {
        let dir = std::env::var(SECRETS_DIR_ENV)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_SECRETS_DIR.to_owned());
        let key = resolve_key(database_url)?;
        Self::open(Path::new(&dir), key)
    }

    /// Explicit constructor for tests and alternative supervisors.
    pub fn open(dir: &Path, key: [u8; KEY_LEN]) -> Result<Self, String> {
        create_private_dir(dir).map_err(|error| {
            format!(
                "secret storage directory {} cannot be secured: {error}",
                dir.display()
            )
        })?;
        Ok(Self {
            dir: dir.to_owned(),
            key,
        })
    }

    fn path_for(&self, reference: &SecretReference) -> PathBuf {
        self.dir.join(reference.name())
    }
}

impl SecretBackend for FileSecretBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::LocalEncrypted
    }

    fn put<'a>(&'a self, reference: &'a SecretReference, value: SecretValue) -> BackendFuture<'a> {
        Box::pin(async move {
            // File IO stays off the async worker thread; material crosses the
            // boundary by owned value only.
            let dir = self.dir.clone();
            let key = self.key;
            let name = reference.name().to_owned();
            let plaintext = value.as_bytes().to_vec();
            tokio::task::spawn_blocking(move || seal_named(&dir, &name, &key, plaintext))
                .await
                .map_err(|_| BackendError)?
                .map(|_| BackendWrite::changed())
                .map_err(|_| BackendError)
        })
    }

    fn delete<'a>(&'a self, reference: &'a SecretReference) -> BackendFuture<'a> {
        Box::pin(async move {
            let path = self.path_for(reference);
            tokio::task::spawn_blocking(move || match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err(()),
            })
            .await
            .map_err(|_| BackendError)?
            .map(|_| BackendWrite::changed())
            .map_err(|_| BackendError)
        })
    }
}

/// Encrypts one value and atomically writes the named reference file.
///
/// `name` is the backend-minted `us-<uuid>` string.
///
/// The on-disk layout is `header \0 nonce || AEAD(value || tag)`; the header
/// stays plaintext so any future decryptor can locate the nonce without the
/// key, and it is bound as AAD so a tampered header fails authentication.
fn seal_named(dir: &Path, name: &str, key: &[u8; KEY_LEN], value: Vec<u8>) -> Result<(), ()> {
    // The strict character check keeps the composed path inside the vault
    // directory regardless of what produced the name.
    if !is_backend_name(name) {
        return Err(());
    }
    let nonce = random_nonce().map_err(|_| ())?;

    let mut header = Vec::with_capacity(FORMAT_TAG.len() + 1 + NONCE_LEN);
    header.extend_from_slice(FORMAT_TAG);
    header.push(0);
    header.extend_from_slice(&nonce);

    let mut sealed = value;
    seal_in_place(key, &nonce, &header, &mut sealed)?;

    let mut blob = header;
    blob.extend_from_slice(&sealed);
    let path = dir.join(name);
    let tmp = dir.join(format!("{name}.tmp"));
    write_private_file(&tmp, &blob)?;
    std::fs::rename(&tmp, &path).map_err(|_| ())
}

/// AES-256-GCM seals `value` in place, appending the 16-byte tag. `aad`
/// authenticates cleartext context (the on-disk header) without encrypting it.
/// `nonce` must be unique per (key, plaintext) pair.
fn seal_in_place(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    value: &mut Vec<u8>,
) -> Result<(), ()> {
    let unbound = UnboundKey::new(&AES_256_GCM, key).map_err(|_| ())?;
    let sealing = LessSafeKey::new(unbound);
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| ())?;
    sealing
        .seal_in_place_append_tag(nonce, Aad::from(aad), value)
        .map_err(|_| ())
}

/// Creates `dir` with owner-only permissions.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        // A mode argument is skipped for a pre-existing directory, so the
        // owner-only requirement is enforced with an explicit chmod too.
        if !dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Creates one `0600` file and durably flushes it before rename.
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), ()> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| ())?;
        file.write_all(bytes).map_err(|_| ())?;
        file.sync_all().map_err(|_| ())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).map_err(|_| ())
    }
}

/// Key from `VOIE_SECRETS_KEY` (hex 32 bytes) or the documented database-URL
/// salt derivation.
fn resolve_key(database_url: &str) -> Result<[u8; KEY_LEN], String> {
    match std::env::var(SECRETS_KEY_ENV) {
        Ok(hex_key) => decode_hex_32(hex_key.trim()).ok_or_else(|| {
            format!("{SECRETS_KEY_ENV} must be exactly 64 hex characters (32 bytes)")
        }),
        Err(std::env::VarError::NotPresent) => {
            let mut hasher = Sha256::new();
            hasher.update(KEY_DERIVATION_SALT);
            hasher.update(database_url.as_bytes());
            let digest = hasher.finalize();
            debug_assert_eq!(digest.len(), SHA256_OUTPUT_LEN);
            let mut key = [0u8; KEY_LEN];
            key.copy_from_slice(&digest);
            Ok(key)
        }
        Err(error) => Err(format!("{SECRETS_KEY_ENV} is unreadable: {error}")),
    }
}

fn decode_hex_32(text: &str) -> Option<[u8; KEY_LEN]> {
    if text.len() != KEY_LEN * 2 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut key = [0u8; KEY_LEN];
    for (index, chunk) in text.as_bytes().chunks(2).enumerate() {
        let high = (chunk[0] as char).to_digit(16).expect("validated hex") as u8;
        let low = (chunk[1] as char).to_digit(16).expect("validated hex") as u8;
        key[index] = high << 4 | low;
    }
    Some(key)
}

fn random_nonce() -> Result<[u8; NONCE_LEN], ring::error::Unspecified> {
    let mut nonce = [0u8; NONCE_LEN];
    SystemRandom::new().fill(&mut nonce)?;
    Ok(nonce)
}

/// Only the backend-minted `us-<uuid>` shape may name a vault file.
fn is_backend_name(name: &str) -> bool {
    name.starts_with("us-")
        && name.len() > 3
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir() -> TempDir {
        let path = std::env::temp_dir().join(format!("voie-file-backend-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("temp dir creates");
        TempDir(path)
    }

    fn reference() -> SecretReference {
        SecretReference::for_test(BackendKind::LocalEncrypted, Uuid::new_v4())
    }

    #[tokio::test]
    async fn open_creates_owner_only_directory() {
        let dir = temp_dir();
        let backend = FileSecretBackend::open(&dir.0, [7u8; KEY_LEN]).expect("backend opens");
        assert_eq!(backend.kind(), BackendKind::LocalEncrypted);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir.0)
                .expect("dir exists")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700, "vault directory is owner-only");
        }
    }

    #[tokio::test]
    async fn put_writes_0600_file_and_delete_removes_it() {
        let dir = temp_dir();
        let backend = FileSecretBackend::open(&dir.0, [9u8; KEY_LEN]).expect("backend opens");
        let reference = reference();

        let value = SecretValue::from_text("file-material").expect("non-empty value");
        let write = backend.put(&reference, value).await.expect("put succeeds");
        assert!(write.changed);

        let path = dir.0.join(reference.name());
        assert!(path.exists(), "material file exists");
        assert!(
            !dir.0.join(format!("{}.tmp", reference.name())).exists(),
            "no torn temp file survives a completed write"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("file exists")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "material file is owner-only");
        }

        let delete = backend.delete(&reference).await.expect("delete succeeds");
        assert!(delete.changed);
        assert!(!path.exists(), "material file is removed");
    }

    #[tokio::test]
    async fn stored_file_is_ciphertext_not_plaintext() {
        let dir = temp_dir();
        let backend = FileSecretBackend::open(&dir.0, [11u8; KEY_LEN]).expect("backend opens");
        let reference = reference();
        let value = SecretValue::from_text("plaintext-marker-value").expect("non-empty value");
        backend.put(&reference, value).await.expect("put succeeds");
        let bytes = std::fs::read(dir.0.join(reference.name())).expect("material file reads");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("plaintext-marker-value"),
            "material file never stores plaintext"
        );
        assert!(
            bytes.starts_with(FORMAT_TAG),
            "material file carries the format tag"
        );
    }

    #[tokio::test]
    async fn delete_of_absent_reference_is_idempotent() {
        let dir = temp_dir();
        let backend = FileSecretBackend::open(&dir.0, [13u8; KEY_LEN]).expect("backend opens");
        let reference = reference();
        let delete = backend.delete(&reference).await.expect("delete succeeds");
        assert!(delete.changed, "absent delete is still a completed state");
    }

    #[test]
    fn seal_named_rejects_non_backend_names() {
        let dir = temp_dir();
        assert!(
            seal_named(&dir.0, "../escape", &[1u8; KEY_LEN], b"x".to_vec()).is_err(),
            "path traversal names are refused"
        );
        assert!(
            seal_named(&dir.0, "us-", &[1u8; KEY_LEN], b"x".to_vec()).is_err(),
            "empty reference bodies are refused"
        );
    }

    #[test]
    fn hex_key_decoding_is_strict() {
        assert!(decode_hex_32(&"ab".repeat(32)).is_some());
        assert!(decode_hex_32("abcd").is_none(), "wrong length refused");
        assert!(decode_hex_32(&"zz".repeat(32)).is_none(), "non-hex refused");
        assert!(decode_hex_32("").is_none());
    }

    #[test]
    fn backend_name_shape_is_enforced() {
        assert!(is_backend_name(&format!("us-{}", Uuid::new_v4())));
        assert!(!is_backend_name("secret"));
        assert!(!is_backend_name("us-../../x"));
    }
}
