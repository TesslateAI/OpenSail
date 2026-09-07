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
//! * `VOIE_SECRETS_KEY` — exactly 64 hex characters (32 bytes), or the same
//!   value in `VOIE_SECRETS_KEY_FILE`. Local-encrypted mode requires this
//!   explicit random key; startup fails if it is missing or malformed.
//! * `VOIE_SECRETS_REKEY_FROM_LEGACY=1` is the only path that may still
//!   derive `SHA-256("voie-secret-vault" || database URL)`: it decrypts
//!   existing vault files, re-seals them with the explicit key, and writes
//!   a marker. Normal runtime never derives a key from deployment data.
//! * `memory` remains explicit test/ephemeral mode. Canonical production
//!   remains Key Vault.
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
use uuid::Uuid;

use super::{
    BackendError, BackendFuture, BackendKind, BackendWrite, SecretBackend, SecretReference,
    SecretValue,
};

const FORMAT_TAG: &[u8] = b"voie-secret-vault-1";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
/// `VOIE_SECRETS_DIR`: material directory. Defaults to `./data/secrets`.
pub const SECRETS_DIR_ENV: &str = "VOIE_SECRETS_DIR";
/// `VOIE_SECRETS_KEY`: hex-encoded 32-byte key. Required for local-encrypted.
pub const SECRETS_KEY_ENV: &str = "VOIE_SECRETS_KEY";
/// `VOIE_SECRETS_KEY_FILE`: 0600 file holding the same 64 hex characters.
pub const SECRETS_KEY_FILE_ENV: &str = "VOIE_SECRETS_KEY_FILE";
/// `VOIE_SECRETS_REKEY_FROM_LEGACY=1`: one-shot rewrite of derived-key vault files.
pub const SECRETS_REKEY_FROM_LEGACY_ENV: &str = "VOIE_SECRETS_REKEY_FROM_LEGACY";
pub const DEFAULT_SECRETS_DIR: &str = "./data/secrets";
const KEY_DERIVATION_SALT: &[u8] = b"voie-secret-vault";
const REKEY_MARKER: &str = ".rekeyed-from-legacy";

/// Durable local material store. One encrypted, owner-only file per backend
/// reference inside an owner-only directory.
#[derive(Debug)]
pub struct FileSecretBackend {
    dir: PathBuf,
    key: [u8; KEY_LEN],
}

impl FileSecretBackend {
    /// Opens (creating if needed) the material directory and resolves the
    /// encryption key from the explicit environment key. `database_url` is
    /// used only for the optional one-shot legacy rekey.
    pub fn from_env(database_url: &str) -> Result<Self, String> {
        let dir = std::env::var(SECRETS_DIR_ENV)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_SECRETS_DIR.to_owned());
        let key = resolve_key()?;
        if rekey_from_legacy_requested() {
            rekey_from_legacy(Path::new(&dir), database_url, &key)?;
        }
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

    pub(crate) async fn get_material(
        &self,
        reference: &SecretReference,
    ) -> Result<SecretValue, BackendError> {
        let dir = self.dir.clone();
        let key = self.key;
        let name = reference.name().to_owned();
        let plaintext = tokio::task::spawn_blocking(move || open_named(&dir, &name, &key))
            .await
            .map_err(|_| BackendError)?
            .map_err(|_| BackendError)?;
        SecretValue::from_bytes(plaintext).map_err(|_| BackendError)
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
    let tmp = unique_temp(dir, name);
    if write_private_file(&tmp, &blob).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err(());
    }
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err(());
    }
    fsync_dir(dir)
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

fn open_named(dir: &Path, name: &str, key: &[u8; KEY_LEN]) -> Result<Vec<u8>, ()> {
    if !is_backend_name(name) {
        return Err(());
    }
    let blob = std::fs::read(dir.join(name)).map_err(|_| ())?;
    let header_len = FORMAT_TAG.len() + 1 + NONCE_LEN;
    if blob.len() <= header_len {
        return Err(());
    }
    let (header, sealed) = blob.split_at(header_len);
    if !header.starts_with(FORMAT_TAG) || header[FORMAT_TAG.len()] != 0 {
        return Err(());
    }
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&header[FORMAT_TAG.len() + 1..]);
    let mut buffer = sealed.to_vec();
    let unbound = UnboundKey::new(&AES_256_GCM, key).map_err(|_| ())?;
    let opening = LessSafeKey::new(unbound);
    let nonce = Nonce::try_assume_unique_for_key(&nonce).map_err(|_| ())?;
    let plaintext = opening
        .open_in_place(nonce, Aad::from(header), &mut buffer)
        .map_err(|_| ())?;
    Ok(plaintext.to_vec())
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

fn unique_temp(dir: &Path, prefix: &str) -> PathBuf {
    dir.join(format!("{prefix}.{}.tmp", Uuid::new_v4().as_simple()))
}

fn fsync_dir(dir: &Path) -> Result<(), ()> {
    let file = std::fs::File::open(dir).map_err(|_| ())?;
    file.sync_all().map_err(|_| ())
}

/// Key from `VOIE_SECRETS_KEY` or `VOIE_SECRETS_KEY_FILE`. Never derived
/// from the database URL on this path.
fn resolve_key() -> Result<[u8; KEY_LEN], String> {
    if let Ok(hex_key) = std::env::var(SECRETS_KEY_ENV) {
        return decode_hex_32(hex_key.trim()).ok_or_else(|| {
            format!("{SECRETS_KEY_ENV} must be exactly 64 hex characters (32 bytes)")
        });
    }
    match std::env::var(SECRETS_KEY_FILE_ENV) {
        Ok(path) => {
            let path = path.trim();
            if path.is_empty() {
                return Err(format!("{SECRETS_KEY_FILE_ENV} is empty"));
            }
            let hex_key = std::fs::read_to_string(path)
                .map_err(|_| format!("{SECRETS_KEY_FILE_ENV} is unreadable"))?;
            decode_hex_32(hex_key.trim()).ok_or_else(|| {
                format!("{SECRETS_KEY_FILE_ENV} must contain exactly 64 hex characters (32 bytes)")
            })
        }
        Err(std::env::VarError::NotPresent) => Err(format!(
            "{SECRETS_KEY_ENV} is required for local-encrypted secret storage"
        )),
        Err(error) => Err(format!("{SECRETS_KEY_FILE_ENV} is unreadable: {error}")),
    }
}

fn rekey_from_legacy_requested() -> bool {
    matches!(
        std::env::var(SECRETS_REKEY_FROM_LEGACY_ENV),
        Ok(value) if value == "1" || value.eq_ignore_ascii_case("true")
    )
}

fn derived_legacy_key(database_url: &str) -> [u8; KEY_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(KEY_DERIVATION_SALT);
    hasher.update(database_url.as_bytes());
    let digest = hasher.finalize();
    debug_assert_eq!(digest.len(), SHA256_OUTPUT_LEN);
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&digest);
    key
}

fn rekey_from_legacy(
    dir: &Path,
    database_url: &str,
    new_key: &[u8; KEY_LEN],
) -> Result<(), String> {
    create_private_dir(dir).map_err(|_| "secret storage directory cannot be secured".to_owned())?;
    let marker = dir.join(REKEY_MARKER);
    let legacy = derived_legacy_key(database_url);
    migrate_vault_objects(dir, &legacy, new_key)?;
    if !vault_readable_with(dir, new_key)? {
        return Err("legacy vault rekey left unreadables with the new key".to_owned());
    }
    write_rekey_marker(&marker)?;
    Ok(())
}

fn migrate_vault_objects(
    dir: &Path,
    legacy: &[u8; KEY_LEN],
    new_key: &[u8; KEY_LEN],
) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Err("secret storage directory is unreadable".to_owned()),
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return Err("secret storage directory is unreadable".to_owned());
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_backend_name(name) {
            continue;
        }
        if open_named(dir, name, new_key).is_ok() {
            continue;
        }
        let plaintext = open_named(dir, name, legacy)
            .map_err(|_| "legacy vault file cannot be decrypted during rekey".to_owned())?;
        seal_named(dir, name, new_key, plaintext)
            .map_err(|_| "legacy vault file cannot be resealed".to_owned())?;
        open_named(dir, name, new_key)
            .map_err(|_| "resealed vault file cannot be read with the new key".to_owned())?;
    }
    Ok(())
}

fn vault_readable_with(dir: &Path, key: &[u8; KEY_LEN]) -> Result<bool, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Err("secret storage directory is unreadable".to_owned()),
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return Err("secret storage directory is unreadable".to_owned());
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_backend_name(name) {
            continue;
        }
        if open_named(dir, name, key).is_err() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn write_rekey_marker(marker: &Path) -> Result<(), String> {
    let dir = marker
        .parent()
        .ok_or_else(|| "legacy rekey marker path is invalid".to_owned())?;
    let tmp = unique_temp(dir, ".rekeyed-from-legacy");
    if write_private_file(&tmp, b"1\n").is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err("legacy rekey marker cannot be written".to_owned());
    }
    if std::fs::rename(&tmp, marker).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err("legacy rekey marker cannot be written".to_owned());
    }
    fsync_dir(dir).map_err(|_| "legacy rekey marker cannot be flushed".to_owned())
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
    async fn put_then_get_round_trips_for_fabric_injection() {
        let dir = temp_dir();
        let backend = FileSecretBackend::open(&dir.0, [17u8; KEY_LEN]).expect("backend opens");
        let reference = reference();
        backend
            .put(
                &reference,
                SecretValue::from_text("round-trip").expect("non-empty"),
            )
            .await
            .expect("put");
        let material = backend.get_material(&reference).await.expect("get");
        assert_eq!(material.as_bytes(), b"round-trip");
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

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn clear_key_env() {
        unsafe {
            std::env::remove_var(SECRETS_KEY_ENV);
            std::env::remove_var(SECRETS_KEY_FILE_ENV);
            std::env::remove_var(SECRETS_REKEY_FROM_LEGACY_ENV);
            std::env::remove_var(SECRETS_DIR_ENV);
        }
    }

    #[test]
    fn missing_or_malformed_explicit_key_fails_startup() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        clear_key_env();
        let err = resolve_key().expect_err("missing key is refused");
        assert!(err.contains(SECRETS_KEY_ENV));
        unsafe {
            std::env::set_var(SECRETS_KEY_ENV, "not-hex");
        }
        assert!(resolve_key().is_err());
        unsafe {
            std::env::set_var(SECRETS_KEY_ENV, "ab");
        }
        assert!(resolve_key().is_err());
        clear_key_env();
    }

    #[tokio::test]
    async fn explicit_key_round_trips_material() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        clear_key_env();
        let dir = temp_dir();
        let key = "ab".repeat(32);
        unsafe {
            std::env::set_var(SECRETS_KEY_ENV, &key);
            std::env::set_var(SECRETS_DIR_ENV, dir.0.to_str().expect("utf8 path"));
        }
        let backend = FileSecretBackend::from_env("postgres://unused").expect("opens");
        let reference = reference();
        backend
            .put(
                &reference,
                SecretValue::from_text("explicit-key-value").expect("non-empty"),
            )
            .await
            .expect("put");
        let material = backend.get_material(&reference).await.expect("get");
        assert_eq!(material.as_bytes(), b"explicit-key-value");
        clear_key_env();
    }

    #[tokio::test]
    async fn legacy_derived_vault_rekeys_and_then_requires_explicit_key() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        clear_key_env();
        let dir = temp_dir();
        let database_url = "postgres://voie:secret@127.0.0.1:5432/voie";
        let legacy = derived_legacy_key(database_url);
        let reference = reference();
        seal_named(
            &dir.0,
            reference.name(),
            &legacy,
            b"legacy-material".to_vec(),
        )
        .expect("legacy seal");
        let key = "cd".repeat(32);
        unsafe {
            std::env::set_var(SECRETS_KEY_ENV, &key);
            std::env::set_var(SECRETS_DIR_ENV, dir.0.to_str().expect("utf8 path"));
            std::env::set_var(SECRETS_REKEY_FROM_LEGACY_ENV, "1");
        }
        let backend = FileSecretBackend::from_env(database_url).expect("rekey opens");
        let material = backend
            .get_material(&reference)
            .await
            .expect("get after rekey");
        assert_eq!(material.as_bytes(), b"legacy-material");
        assert!(dir.0.join(REKEY_MARKER).exists());
        unsafe {
            std::env::remove_var(SECRETS_REKEY_FROM_LEGACY_ENV);
        }
        let again = FileSecretBackend::from_env(database_url).expect("explicit key still opens");
        let material = again.get_material(&reference).await.expect("get");
        assert_eq!(material.as_bytes(), b"legacy-material");
        unsafe {
            std::env::remove_var(SECRETS_KEY_ENV);
        }
        assert!(
            FileSecretBackend::from_env(database_url).is_err(),
            "derived-key startup is refused after migration"
        );
        clear_key_env();
    }

    #[test]
    fn mixed_vault_rekeys_remaining_legacy_objects() {
        let dir = temp_dir();
        let database_url = "postgres://voie:mixed@127.0.0.1:5432/voie";
        let legacy = derived_legacy_key(database_url);
        let new_key = [21u8; KEY_LEN];
        let old_ref = format!("us-{}", Uuid::new_v4());
        let new_ref = format!("us-{}", Uuid::new_v4());
        seal_named(&dir.0, &old_ref, &legacy, b"old-object".to_vec()).expect("legacy seal");
        seal_named(&dir.0, &new_ref, &new_key, b"new-object".to_vec()).expect("new seal");
        rekey_from_legacy(&dir.0, database_url, &new_key).expect("mixed rekey");
        assert_eq!(
            open_named(&dir.0, &old_ref, &new_key).expect("old now new"),
            b"old-object"
        );
        assert_eq!(
            open_named(&dir.0, &new_ref, &new_key).expect("new remains"),
            b"new-object"
        );
    }

    #[test]
    fn stale_temp_residue_does_not_block_rekey_retry() {
        let dir = temp_dir();
        let database_url = "postgres://voie:retry@127.0.0.1:5432/voie";
        let legacy = derived_legacy_key(database_url);
        let new_key = [22u8; KEY_LEN];
        let name = format!("us-{}", Uuid::new_v4());
        seal_named(&dir.0, &name, &legacy, b"retry-object".to_vec()).expect("legacy seal");
        std::fs::write(dir.0.join(format!("{name}.tmp")), b"stale-fixed-tmp").expect("stale tmp");
        std::fs::write(dir.0.join(format!("{name}.1.dead.tmp")), b"stale-unique")
            .expect("unique tmp");
        rekey_from_legacy(&dir.0, database_url, &new_key).expect("retry despite residue");
        assert_eq!(
            open_named(&dir.0, &name, &new_key).expect("migrated"),
            b"retry-object"
        );
    }

    #[test]
    fn marker_with_legacy_object_does_not_false_pass() {
        let dir = temp_dir();
        let database_url = "postgres://voie:marker@127.0.0.1:5432/voie";
        let legacy = derived_legacy_key(database_url);
        let new_key = [23u8; KEY_LEN];
        let migrated = format!("us-{}", Uuid::new_v4());
        let leftover = format!("us-{}", Uuid::new_v4());
        seal_named(&dir.0, &migrated, &new_key, b"already-new".to_vec()).expect("new seal");
        seal_named(&dir.0, &leftover, &legacy, b"still-old".to_vec()).expect("legacy seal");
        std::fs::write(dir.0.join(REKEY_MARKER), b"1\n").expect("marker");
        rekey_from_legacy(&dir.0, database_url, &new_key).expect("resumes from false marker");
        assert_eq!(
            open_named(&dir.0, &leftover, &new_key).expect("leftover migrated"),
            b"still-old"
        );
        assert!(open_named(&dir.0, &leftover, &legacy).is_err());
    }

    #[test]
    fn backend_name_shape_is_enforced() {
        assert!(is_backend_name(&format!("us-{}", Uuid::new_v4())));
        assert!(!is_backend_name("secret"));
        assert!(!is_backend_name("us-../../x"));
    }
}
