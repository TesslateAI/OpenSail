//! Fixed guest helper that packages an Application root into deterministic tar.zst.

use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tar::{Builder, Header};

const MAX_FILES: usize = 20_000;
const RELEASE_MAX_BYTES: u64 = 512 * 1024 * 1024;
const SNAPSHOT_MAX_BYTES: u64 = 64 * 1024 * 1024 * 1024;

const RELEASE_EXCLUDES: &[&str] = &[
    ".git",
    ".voie/tmp",
    "node_modules",
    "target",
    "__pycache__",
    ".pytest_cache",
    ".coverage",
    "coverage",
];

const SNAPSHOT_EXCLUDES: &[&str] = &[
    ".voie/tmp",
    "node_modules",
    "target",
    "__pycache__",
    ".pytest_cache",
    ".coverage",
    "coverage",
];

#[derive(Debug)]
pub enum PackError {
    Io,
    Root,
    Path,
    Symlink,
    Special,
    Limit,
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackError::Io => write!(f, "pack i/o failed"),
            PackError::Root => write!(f, "application root is invalid"),
            PackError::Path => write!(f, "pack path escaped the application root"),
            PackError::Symlink => write!(f, "pack rejected an escaping symlink"),
            PackError::Special => write!(f, "pack rejected a special file"),
            PackError::Limit => write!(f, "pack exceeded file or byte limits"),
        }
    }
}

impl std::error::Error for PackError {}

impl From<io::Error> for PackError {
    fn from(_: io::Error) -> Self {
        PackError::Io
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackResult {
    pub artifact: Vec<u8>,
    pub artifact_hash: [u8; 32],
    pub file_count: u64,
    pub byte_length: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackKind {
    Release,
    WorkspaceSnapshot,
}

impl PackKind {
    fn excludes(self) -> &'static [&'static str] {
        match self {
            PackKind::Release => RELEASE_EXCLUDES,
            PackKind::WorkspaceSnapshot => SNAPSHOT_EXCLUDES,
        }
    }

    fn max_bytes(self) -> u64 {
        match self {
            PackKind::Release => RELEASE_MAX_BYTES,
            PackKind::WorkspaceSnapshot => SNAPSHOT_MAX_BYTES,
        }
    }
}

/// Pack `root` relative to `application_root`. Paths are rejected if they
/// leave the Application root, are absolute, or contain `..`.
pub fn pack(application_root: &Path, root_path: &str) -> Result<PackResult, PackError> {
    pack_kind(application_root, root_path, PackKind::Release)
}

fn pack_kind(
    application_root: &Path,
    root_path: &str,
    kind: PackKind,
) -> Result<PackResult, PackError> {
    let mut artifact = Vec::new();
    let meta = pack_into(application_root, root_path, kind, &mut artifact)?;
    Ok(PackResult {
        artifact,
        artifact_hash: meta.artifact_hash,
        file_count: meta.file_count,
        byte_length: meta.byte_length,
    })
}

struct PackMeta {
    artifact_hash: [u8; 32],
    file_count: u64,
    byte_length: u64,
}

struct HashingWriter<W: Write> {
    inner: W,
    hasher: Sha256,
    written: u64,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.written = self.written.saturating_add(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn pack_into<W: Write>(
    application_root: &Path,
    root_path: &str,
    kind: PackKind,
    dest: W,
) -> Result<PackMeta, PackError> {
    let source = resolve_source(application_root, root_path)?;
    let files = collect_sorted(&source, kind)?;
    let hashed = HashingWriter {
        inner: dest,
        hasher: Sha256::new(),
        written: 0,
    };
    let encoder = zstd::stream::write::Encoder::new(hashed, 3).map_err(|_| PackError::Io)?;
    let encoder = encode_tar(&files, kind.max_bytes(), encoder)?;
    let hashed = encoder.finish().map_err(|_| PackError::Io)?;
    Ok(PackMeta {
        artifact_hash: hashed.hasher.finalize().into(),
        file_count: files.len() as u64,
        byte_length: hashed.written,
    })
}

fn resolve_source(application_root: &Path, root_path: &str) -> Result<PathBuf, PackError> {
    let application_root = application_root
        .canonicalize()
        .map_err(|_| PackError::Root)?;
    if root_path.starts_with('/') || root_path.contains('\0') {
        return Err(PackError::Path);
    }
    if root_path == "." {
        return Ok(application_root);
    }
    let joined = application_root.join(root_path);
    let canonical = joined.canonicalize().map_err(|_| PackError::Root)?;
    if !canonical.starts_with(&application_root) {
        return Err(PackError::Path);
    }
    Ok(canonical)
}

fn collect_sorted(source: &Path, kind: PackKind) -> Result<Vec<Entry>, PackError> {
    let ignore = read_ignore(source);
    let mut files = Vec::new();
    collect(source, source, &ignore, kind.excludes(), &mut files)?;
    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    if files.len() > MAX_FILES {
        return Err(PackError::Limit);
    }
    Ok(files)
}

fn encode_tar<W: Write>(
    files: &[Entry],
    max_bytes: u64,
    writer: W,
) -> Result<W, PackError> {
    let mut builder = Builder::new(writer);
    builder.mode(tar::HeaderMode::Deterministic);
    let mut total = 0u64;
    for file in files {
        total = total.saturating_add(file.size);
        if total > max_bytes {
            return Err(PackError::Limit);
        }
        let mut header = Header::new_ustar();
        header
            .set_path(&file.relative)
            .map_err(|_| PackError::Path)?;
        header.set_size(file.size);
        header.set_mode(file.mode);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        let mut contents = File::open(&file.absolute)?;
        builder.append(&header, &mut contents)?;
    }
    builder.into_inner().map_err(|_| PackError::Io)
}

/// Guest staging path, inside the hard-excluded `.voie/tmp` directory.
pub const STAGED_ARTIFACT: &str = ".voie/tmp/release.tar.zst";
pub const STAGED_SNAPSHOT: &str = ".voie/tmp/workspace-snapshot.tar.zst";

/// Packs the Application root and writes the artifact where Fabric can copy
/// it from the guest. Project commands stay in the guest; the host never
/// runs this against a control or Fabric working tree.
pub fn pack_and_stage(application_root: &Path, root_path: &str) -> Result<PackResult, PackError> {
    let result = pack(application_root, root_path)?;
    let source = if root_path == "." {
        application_root
            .canonicalize()
            .map_err(|_| PackError::Root)?
    } else {
        application_root
            .join(root_path)
            .canonicalize()
            .map_err(|_| PackError::Root)?
    };
    let staged = source.join(STAGED_ARTIFACT);
    if let Some(parent) = staged.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&staged, &result.artifact)?;
    Ok(result)
}

/// Workspace snapshot includes `.git` and writes to a staged file so the
/// Fabric host can copy it. Scratch directories stay excluded.
pub fn snapshot_and_stage(workspace_root: &Path) -> Result<PackResult, PackError> {
    let source = workspace_root
        .canonicalize()
        .map_err(|_| PackError::Root)?;
    let staged = source.join(STAGED_SNAPSHOT);
    if let Some(parent) = staged.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(&staged)?;
    let meta = pack_into(workspace_root, ".", PackKind::WorkspaceSnapshot, file)?;
    Ok(PackResult {
        artifact: Vec::new(),
        artifact_hash: meta.artifact_hash,
        file_count: meta.file_count,
        byte_length: meta.byte_length,
    })
}

struct Entry {
    relative: String,
    absolute: PathBuf,
    size: u64,
    mode: u32,
}

fn collect(
    source: &Path,
    current: &Path,
    ignore: &[String],
    hard_excludes: &[&str],
    out: &mut Vec<Entry>,
) -> Result<(), PackError> {
    let mut entries: Vec<_> = fs::read_dir(current)?
        .filter_map(|entry| entry.ok())
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        let relative = path
            .strip_prefix(source)
            .map_err(|_| PackError::Path)?
            .to_string_lossy()
            .replace('\\', "/");
        if relative.contains("..") || relative.starts_with('/') {
            return Err(PackError::Path);
        }
        if excluded(&relative, ignore, hard_excludes) {
            continue;
        }
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            let target = fs::read_link(&path)?;
            if target.is_absolute() {
                return Err(PackError::Symlink);
            }
            let resolved = path.parent().unwrap_or(source).join(target);
            let canonical = match resolved.canonicalize() {
                Ok(path) => path,
                Err(_) => return Err(PackError::Symlink),
            };
            if !canonical.starts_with(source) {
                return Err(PackError::Symlink);
            }
            return Err(PackError::Special);
        }
        if file_type.is_dir() {
            collect(source, &path, ignore, hard_excludes, out)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(PackError::Special);
        }
        out.push(Entry {
            relative,
            absolute: path,
            size: metadata.len(),
            mode: metadata.permissions().mode() & 0o755,
        });
        if out.len() > MAX_FILES {
            return Err(PackError::Limit);
        }
    }
    Ok(())
}

fn excluded(relative: &str, ignore: &[String], hard_excludes: &[&str]) -> bool {
    for prefix in hard_excludes {
        if relative == *prefix || relative.starts_with(&format!("{prefix}/")) {
            return true;
        }
    }
    ignore
        .iter()
        .any(|rule| relative == rule || relative.starts_with(&format!("{rule}/")))
}

fn read_ignore(source: &Path) -> Vec<String> {
    let path = source.join(".voieignore");
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.trim_end_matches('/').to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn tempdir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "voie-pack-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn packs_relative_files_and_hashes() {
        let root = tempdir();
        fs::write(root.join("app.js"), b"ok").unwrap();
        let packed = pack(&root, ".").expect("pack succeeds");
        assert_eq!(packed.file_count, 1);
        assert_eq!(packed.byte_length, packed.artifact.len() as u64);
        assert_ne!(packed.artifact_hash, [0u8; 32]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_escaping_symlink() {
        let root = tempdir();
        let outside = tempdir();
        fs::write(outside.join("secret"), b"no").unwrap();
        symlink(outside.join("secret"), root.join("link")).unwrap();
        assert!(pack(&root, ".").is_err());
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn excludes_git_and_caches() {
        let root = tempdir();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), b"x").unwrap();
        fs::write(root.join("keep.txt"), b"y").unwrap();
        let packed = pack(&root, ".").expect("pack succeeds");
        assert_eq!(packed.file_count, 1);
        let staged = pack_and_stage(&root, ".").expect("stages artifact");
        assert_eq!(staged.artifact_hash, packed.artifact_hash);
        let written = fs::read(root.join(STAGED_ARTIFACT)).expect("staged file");
        assert_eq!(written, staged.artifact);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_snapshot_keeps_git() {
        let root = tempdir();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), b"x").unwrap();
        fs::write(root.join("keep.txt"), b"y").unwrap();
        let snapshot = snapshot_and_stage(&root).expect("snapshot succeeds");
        assert_eq!(snapshot.file_count, 2);
        let staged = root.join(STAGED_SNAPSHOT);
        assert!(staged.exists());
        assert_eq!(
            snapshot.byte_length,
            fs::metadata(&staged).expect("staged snapshot").len()
        );
        assert!(snapshot.artifact.is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
