//! Per-project filesystem isolation.
//!
//! Every path the agent supplies is untrusted. `Sandbox::resolve` is the single
//! chokepoint: nothing in this crate opens an agent-supplied path without going
//! through it. The rules are deliberately strict — reject rather than normalise,
//! because a silently "fixed" path is how escapes happen.
//!
//! Defence layers, in order:
//!   1. Lexical — reject absolute paths, drive prefixes, and any `..` component
//!      *before* touching the filesystem.
//!   2. Symlink — walk the path component by component; any existing component
//!      that is a symlink is rejected outright. This is what stops a symlink
//!      planted inside the sandbox from pointing out of it.
//!   3. Containment — canonicalise the deepest existing ancestor and re-verify
//!      it is still under the canonical root (defends against TOCTOU races and
//!      anything the first two layers missed).
//!   4. Quota — file size, total sandbox size, and file count.

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use crate::error::{Result, VonError};

/// Largest single file the agent may write (8 MiB). Generated source, data and
/// reports are far below this; the cap exists to stop a runaway loop from
/// filling the user's storage.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Total bytes one project sandbox may occupy (256 MiB).
pub const MAX_SANDBOX_BYTES: u64 = 256 * 1024 * 1024;
/// Total number of files one project sandbox may contain.
pub const MAX_SANDBOX_FILES: usize = 4_000;
/// Depth cap on the file tree, both for quota and to bound UI recursion.
pub const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileNode {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub modified_ms: i64,
    pub children: Vec<FileNode>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SandboxUsage {
    pub total_bytes: u64,
    pub file_count: usize,
}

/// A rooted, validated view of one project's directory.
#[derive(Debug, Clone)]
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// Opens (creating if needed) the sandbox rooted at `root`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        fs::create_dir_all(root)?;
        let root = canonical(root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The one chokepoint. Maps an agent-supplied relative path to an absolute
    /// path proven to live inside this sandbox.
    ///
    /// `rel` must be relative and `..`-free. The returned path may not exist
    /// yet (that is the normal case for a write), but every component of it
    /// that *does* exist has been checked.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let rel = rel.trim();
        if rel.is_empty() {
            return Err(VonError::InvalidArgument("empty path".into()));
        }
        if rel.len() > 1024 {
            return Err(VonError::InvalidArgument("path too long".into()));
        }
        // NUL bytes truncate paths in the C layer underneath std::fs.
        if rel.contains('\0') {
            return Err(VonError::PermissionDenied(
                "path contains a NUL byte".into(),
            ));
        }

        // Layer 1: lexical. Normalise separators first so a Windows-style path
        // in a test can't sneak past a POSIX-only check.
        let normalised = rel.replace('\\', "/");
        let candidate = Path::new(&normalised);

        let mut safe = PathBuf::new();
        for component in candidate.components() {
            match component {
                Component::Normal(part) => safe.push(part),
                // `./` is harmless noise; drop it.
                Component::CurDir => {}
                Component::ParentDir => {
                    return Err(VonError::PermissionDenied(format!(
                        "'{rel}' escapes the sandbox with '..'"
                    )))
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(VonError::PermissionDenied(format!(
                        "'{rel}' is absolute; use a path relative to the project root"
                    )))
                }
            }
        }
        if safe.as_os_str().is_empty() {
            return Err(VonError::InvalidArgument(format!(
                "'{rel}' does not name a file"
            )));
        }
        if safe.components().count() > MAX_DEPTH {
            return Err(VonError::QuotaExceeded(format!(
                "'{rel}' nests deeper than {MAX_DEPTH} levels"
            )));
        }

        // Layer 2: symlink walk. Check every existing prefix of the path.
        let mut walked = self.root.clone();
        for component in safe.components() {
            walked.push(component);
            match fs::symlink_metadata(&walked) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(VonError::PermissionDenied(format!(
                        "'{rel}' passes through a symlink, which is not allowed \
                         inside a sandbox"
                    )))
                }
                // Not existing yet is fine — this is a create.
                Ok(_) | Err(_) => {}
            }
        }

        let full = self.root.join(&safe);

        // Layer 3: containment re-check against the canonical filesystem.
        let existing_ancestor = full
            .ancestors()
            .find(|p| p.exists())
            .unwrap_or(self.root.as_path());
        let canonical_ancestor = canonical(existing_ancestor)?;
        if !canonical_ancestor.starts_with(&self.root) {
            return Err(VonError::PermissionDenied(format!(
                "'{rel}' resolves outside the project sandbox"
            )));
        }

        Ok(full)
    }

    /// Converts an absolute path back to the sandbox-relative form the agent and
    /// UI speak in (always `/`-separated).
    pub fn relativise(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    // ---- file operations -------------------------------------------------

    pub fn read_file(&self, rel: &str) -> Result<String> {
        let path = self.resolve(rel)?;
        if !path.exists() {
            return Err(VonError::NotFound(format!("'{rel}' does not exist")));
        }
        if path.is_dir() {
            return Err(VonError::InvalidArgument(format!("'{rel}' is a directory")));
        }
        let bytes = fs::read(&path)?;
        // Binary files would otherwise blow up the model's context with mojibake.
        String::from_utf8(bytes).map_err(|_| {
            VonError::InvalidArgument(format!("'{rel}' is not valid UTF-8 text"))
        })
    }

    pub fn read_bytes(&self, rel: &str) -> Result<Vec<u8>> {
        let path = self.resolve(rel)?;
        if !path.exists() {
            return Err(VonError::NotFound(format!("'{rel}' does not exist")));
        }
        Ok(fs::read(&path)?)
    }

    pub fn write_file(&self, rel: &str, content: &str) -> Result<u64> {
        self.write_bytes(rel, content.as_bytes())
    }

    pub fn write_bytes(&self, rel: &str, content: &[u8]) -> Result<u64> {
        let len = content.len() as u64;
        if len > MAX_FILE_BYTES {
            return Err(VonError::QuotaExceeded(format!(
                "'{rel}' is {len} bytes; the per-file limit is {MAX_FILE_BYTES}"
            )));
        }
        let path = self.resolve(rel)?;

        // Charge the quota against the *delta*, so overwriting a large file with
        // a smaller one always succeeds.
        let previous = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let usage = self.usage()?;
        let projected = usage.total_bytes.saturating_sub(previous).saturating_add(len);
        if projected > MAX_SANDBOX_BYTES {
            return Err(VonError::QuotaExceeded(format!(
                "writing '{rel}' would put this project at {projected} bytes, over \
                 the {MAX_SANDBOX_BYTES} byte limit"
            )));
        }
        if previous == 0 && usage.file_count >= MAX_SANDBOX_FILES {
            return Err(VonError::QuotaExceeded(format!(
                "this project already holds {MAX_SANDBOX_FILES} files"
            )));
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write to a temp file in the same directory, then rename. A crashed or
        // throttled write never leaves a half-written source file for the agent
        // to read back and get confused by.
        let tmp = path.with_extension(format!(
            "{}.tmp{}",
            path.extension().and_then(|e| e.to_str()).unwrap_or(""),
            std::process::id()
        ));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(content)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        Ok(len)
    }

    pub fn append_file(&self, rel: &str, content: &str) -> Result<u64> {
        let existing = if self.resolve(rel)?.exists() {
            self.read_file(rel)?
        } else {
            String::new()
        };
        let combined = format!("{existing}{content}");
        self.write_file(rel, &combined)
    }

    pub fn delete(&self, rel: &str) -> Result<()> {
        let path = self.resolve(rel)?;
        if !path.exists() {
            return Err(VonError::NotFound(format!("'{rel}' does not exist")));
        }
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    pub fn create_dir(&self, rel: &str) -> Result<()> {
        let path = self.resolve(rel)?;
        fs::create_dir_all(&path)?;
        Ok(())
    }

    pub fn exists(&self, rel: &str) -> bool {
        self.resolve(rel).map(|p| p.exists()).unwrap_or(false)
    }

    /// Flat listing of every file, sandbox-relative. Used to give the agent its
    /// bearings on resume.
    pub fn list_flat(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(&self.root)
            .max_depth(MAX_DEPTH)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                out.push(self.relativise(entry.path()));
            }
        }
        out.sort();
        Ok(out)
    }

    /// Nested tree for the Sandbox Explorer UI, directories first then
    /// alphabetical — the ordering the file tree renders in.
    pub fn tree(&self) -> Result<Vec<FileNode>> {
        self.tree_at(&self.root.clone(), 0)
    }

    fn tree_at(&self, dir: &Path, depth: usize) -> Result<Vec<FileNode>> {
        if depth >= MAX_DEPTH {
            return Ok(Vec::new());
        }
        let mut nodes = Vec::new();
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(nodes),
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            // Never surface symlinks in the tree; they cannot be opened anyway.
            if meta.file_type().is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name.contains(".tmp") {
                continue;
            }
            let is_dir = meta.is_dir();
            nodes.push(FileNode {
                path: self.relativise(&path),
                name,
                is_dir,
                size_bytes: if is_dir { 0 } else { meta.len() },
                modified_ms: modified_ms(&meta),
                children: if is_dir {
                    self.tree_at(&path, depth + 1)?
                } else {
                    Vec::new()
                },
            });
        }
        nodes.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        Ok(nodes)
    }

    pub fn usage(&self) -> Result<SandboxUsage> {
        let mut usage = SandboxUsage::default();
        for entry in walkdir::WalkDir::new(&self.root)
            .max_depth(MAX_DEPTH)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                if let Ok(meta) = entry.metadata() {
                    usage.total_bytes += meta.len();
                    usage.file_count += 1;
                }
            }
        }
        Ok(usage)
    }

    /// Zips the whole sandbox to `dest` for the "Download Project" button.
    pub fn export_zip(&self, dest: &Path) -> Result<u64> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::File::create(dest)?;
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);

        for entry in walkdir::WalkDir::new(&self.root)
            .max_depth(MAX_DEPTH)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let rel = self.relativise(entry.path());
            if rel.is_empty() {
                continue;
            }
            if entry.file_type().is_file() {
                zip.start_file(&rel, options)?;
                let bytes = fs::read(entry.path())?;
                std::io::Write::write_all(&mut zip, &bytes)?;
            } else if entry.file_type().is_dir() {
                zip.add_directory(&rel, options)?;
            }
        }
        zip.finish()?;
        Ok(fs::metadata(dest)?.len())
    }
}

/// `fs::canonicalize` with Windows verbatim (`\\?\`) prefixes stripped, so
/// `starts_with` comparisons behave the same on a dev machine as on Android.
fn canonical(path: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    #[cfg(windows)]
    {
        let s = canonical.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(stripped));
        }
    }
    Ok(canonical)
}

fn modified_ms(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> (tempfile::TempDir, Sandbox) {
        let dir = tempfile::tempdir().unwrap();
        let sb = Sandbox::open(dir.path().join("proj")).unwrap();
        (dir, sb)
    }

    #[test]
    fn accepts_ordinary_relative_paths() {
        let (_d, sb) = sandbox();
        assert!(sb.resolve("main.py").is_ok());
        assert!(sb.resolve("src/lib/util.rs").is_ok());
        assert!(sb.resolve("./notes.md").is_ok());
    }

    #[test]
    fn rejects_parent_traversal() {
        let (_d, sb) = sandbox();
        for evil in [
            "../secrets.txt",
            "..",
            "src/../../escape.txt",
            "a/b/../../../../etc/passwd",
            r"..\windows\system32",
        ] {
            let err = sb.resolve(evil).unwrap_err();
            assert!(
                matches!(err, VonError::PermissionDenied(_)),
                "{evil} should be denied, got {err:?}"
            );
        }
    }

    #[test]
    fn rejects_absolute_paths() {
        let (_d, sb) = sandbox();
        for evil in ["/etc/passwd", "/data/data/com.other.app/f", r"C:\Windows"] {
            assert!(
                matches!(sb.resolve(evil), Err(VonError::PermissionDenied(_))),
                "{evil} should be denied"
            );
        }
    }

    #[test]
    fn rejects_nul_and_empty() {
        let (_d, sb) = sandbox();
        assert!(sb.resolve("").is_err());
        assert!(sb.resolve("   ").is_err());
        assert!(sb.resolve("a\0b").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_paths_through_a_symlink() {
        let (dir, sb) = sandbox();
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "leak").unwrap();
        std::os::unix::fs::symlink(&outside, sb.root().join("link")).unwrap();

        assert!(matches!(
            sb.resolve("link/secret.txt"),
            Err(VonError::PermissionDenied(_))
        ));
        assert!(matches!(sb.resolve("link"), Err(VonError::PermissionDenied(_))));
    }

    #[test]
    fn round_trips_content() {
        let (_d, sb) = sandbox();
        sb.write_file("src/main.py", "print('hi')").unwrap();
        assert_eq!(sb.read_file("src/main.py").unwrap(), "print('hi')");
        assert!(sb.exists("src/main.py"));
        assert_eq!(sb.list_flat().unwrap(), vec!["src/main.py".to_string()]);
    }

    #[test]
    fn enforces_per_file_quota() {
        let (_d, sb) = sandbox();
        let huge = "x".repeat(MAX_FILE_BYTES as usize + 1);
        assert!(matches!(
            sb.write_file("big.txt", &huge),
            Err(VonError::QuotaExceeded(_))
        ));
    }

    #[test]
    fn overwriting_does_not_double_charge_quota() {
        let (_d, sb) = sandbox();
        let chunk = "y".repeat(1024 * 1024);
        for _ in 0..8 {
            sb.write_file("same.txt", &chunk).unwrap();
        }
        assert_eq!(sb.usage().unwrap().file_count, 1);
    }

    #[test]
    fn tree_lists_dirs_before_files() {
        let (_d, sb) = sandbox();
        sb.write_file("z.txt", "a").unwrap();
        sb.write_file("src/a.py", "b").unwrap();
        let tree = sb.tree().unwrap();
        assert!(tree[0].is_dir);
        assert_eq!(tree[0].name, "src");
        assert_eq!(tree[1].name, "z.txt");
    }

    #[test]
    fn export_zip_contains_files() {
        let (dir, sb) = sandbox();
        sb.write_file("index.html", "<h1>hi</h1>").unwrap();
        let dest = dir.path().join("out.zip");
        let size = sb.export_zip(&dest).unwrap();
        assert!(size > 0);
        assert!(dest.exists());
    }
}
