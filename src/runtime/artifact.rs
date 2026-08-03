use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

static NEXT_BUNDLED_RUNTIME_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) struct RuntimeArchive {
    path: PathBuf,
    cleanup_dir: Option<PathBuf>,
}

impl RuntimeArchive {
    pub(crate) fn external(path: PathBuf) -> Self {
        Self {
            path,
            cleanup_dir: None,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RuntimeArchive {
    fn drop(&mut self) {
        let Some(dir) = self.cleanup_dir.as_ref() else {
            return;
        };
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(dir);
    }
}

pub(crate) fn materialize_bundled_runtime(bytes: &[u8]) -> Result<RuntimeArchive, String> {
    if !bytes.starts_with(b"!<arch>\n") {
        return Err("bundled libarmfortas_rt.a is not a valid archive".into());
    }

    let base = std::env::temp_dir();
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for _ in 0..128 {
        let id = NEXT_BUNDLED_RUNTIME_ID.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!(
            "armfortas-runtime-{}-{timestamp}-{id}",
            std::process::id()
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(format!(
                    "cannot create bundled runtime directory '{}': {err}",
                    dir.display()
                ));
            }
        }

        let path = dir.join("libarmfortas_rt.a");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = options
            .open(&path)
            .and_then(|mut file| file.write_all(bytes));
        if let Err(err) = result {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_dir(&dir);
            return Err(format!(
                "cannot write bundled runtime archive '{}': {err}",
                path.display()
            ));
        }
        return Ok(RuntimeArchive {
            path,
            cleanup_dir: Some(dir),
        });
    }

    Err(format!(
        "cannot create a unique bundled runtime directory under '{}'",
        base.display()
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeProfile {
    Debug,
    Release,
}

impl RuntimeProfile {
    pub(crate) const fn current() -> Self {
        if cfg!(debug_assertions) {
            Self::Debug
        } else {
            Self::Release
        }
    }

    fn directory(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }

    pub(crate) fn cargo_build_args(self) -> &'static [&'static str] {
        match self {
            Self::Debug => &["build", "-p", "armfortas-rt"],
            Self::Release => &["build", "-p", "armfortas-rt", "--release"],
        }
    }
}

pub(crate) fn runtime_lib_candidate(workspace_root: &Path, profile: RuntimeProfile) -> PathBuf {
    runtime_lib_candidate_from(
        workspace_root,
        profile,
        std::env::var_os("CARGO_TARGET_DIR").as_deref(),
    )
}

fn runtime_lib_candidate_from(
    workspace_root: &Path,
    profile: RuntimeProfile,
    configured_target: Option<&OsStr>,
) -> PathBuf {
    cargo_target_dir_from(workspace_root, configured_target)
        .join(profile.directory())
        .join("libarmfortas_rt.a")
}

fn cargo_target_dir_from(workspace_root: &Path, configured: Option<&OsStr>) -> PathBuf {
    let Some(configured) = configured.filter(|value| !value.is_empty()) else {
        return workspace_root.join("target");
    };
    let configured = PathBuf::from(configured);
    if configured.is_absolute() {
        configured
    } else {
        workspace_root.join(configured)
    }
}

pub(crate) fn fresh_runtime_lib(workspace_root: &Path, profile: RuntimeProfile) -> Option<PathBuf> {
    fresh_runtime_lib_from(
        workspace_root,
        profile,
        std::env::var_os("CARGO_TARGET_DIR").as_deref(),
    )
}

fn fresh_runtime_lib_from(
    workspace_root: &Path,
    profile: RuntimeProfile,
    configured_target: Option<&OsStr>,
) -> Option<PathBuf> {
    let source_mtime = newest_mtime(&workspace_root.join("runtime"))?;
    let candidate = runtime_lib_candidate_from(workspace_root, profile, configured_target);
    fs::metadata(&candidate)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .filter(|mtime| *mtime >= source_mtime)
        .map(|_| candidate)
}

fn newest_mtime(path: &Path) -> Option<SystemTime> {
    let meta = fs::metadata(path).ok()?;
    let mut newest = meta.modified().ok()?;
    if meta.is_dir() {
        for entry in fs::read_dir(path).ok()? {
            let child = newest_mtime(&entry.ok()?.path())?;
            if child > newest {
                newest = child;
            }
        }
    }
    Some(newest)
}

pub(crate) fn find_source_workspace_from(bases: &[PathBuf]) -> Option<PathBuf> {
    for base in bases {
        for ancestor in base.ancestors() {
            if is_armfortas_source_workspace(ancestor) {
                return Some(ancestor.to_path_buf());
            }
        }
    }
    None
}

fn is_armfortas_source_workspace(root: &Path) -> bool {
    manifest_package_name(&root.join("Cargo.toml")).as_deref() == Some("armfortas")
        && manifest_package_name(&root.join("runtime/Cargo.toml")).as_deref()
            == Some("armfortas-rt")
        && root.join("src/lib.rs").is_file()
        && root.join("src/driver/mod.rs").is_file()
        && root.join("runtime/src/lib.rs").is_file()
}

fn manifest_package_name(path: &Path) -> Option<String> {
    let manifest = fs::read_to_string(path).ok()?;
    let mut in_package = false;
    for raw_line in manifest.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_package = line
                .strip_prefix('[')
                .and_then(|section| section.split_once(']'))
                .is_some_and(|(section, _)| section.trim() == "package");
            continue;
        }
        if !in_package {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "name" {
            continue;
        }
        let value = value.trim();
        let quote = value.as_bytes().first().copied()?;
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        let value = &value[1..];
        let end = value.as_bytes().iter().position(|byte| *byte == quote)?;
        return Some(value[..end].to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn temp_root(case: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "armfortas-runtime-artifact-{}-{case}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn profile_selects_only_its_matching_runtime() {
        let root = temp_root("profile");
        let runtime_dir = root.join("runtime");
        fs::create_dir_all(&runtime_dir).unwrap();
        fs::write(runtime_dir.join("Cargo.toml"), b"[package]\nname='rt'\n").unwrap();
        std::thread::sleep(Duration::from_millis(20));

        let debug = runtime_lib_candidate_from(&root, RuntimeProfile::Debug, None);
        let release = runtime_lib_candidate_from(&root, RuntimeProfile::Release, None);
        fs::create_dir_all(debug.parent().unwrap()).unwrap();
        fs::create_dir_all(release.parent().unwrap()).unwrap();
        fs::write(&debug, b"debug").unwrap();
        fs::write(&release, b"release").unwrap();

        assert_eq!(
            fresh_runtime_lib_from(&root, RuntimeProfile::Debug, None),
            Some(debug)
        );
        assert_eq!(
            fresh_runtime_lib_from(&root, RuntimeProfile::Release, None),
            Some(release)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn profile_never_falls_back_to_the_other_archive() {
        let root = temp_root("no-fallback");
        let runtime_dir = root.join("runtime");
        fs::create_dir_all(&runtime_dir).unwrap();
        fs::write(runtime_dir.join("Cargo.toml"), b"[package]\nname='rt'\n").unwrap();
        std::thread::sleep(Duration::from_millis(20));

        let debug = runtime_lib_candidate_from(&root, RuntimeProfile::Debug, None);
        fs::create_dir_all(debug.parent().unwrap()).unwrap();
        fs::write(&debug, b"debug").unwrap();

        assert_eq!(
            fresh_runtime_lib_from(&root, RuntimeProfile::Release, None),
            None
        );
        assert_eq!(
            RuntimeProfile::Release.cargo_build_args(),
            &["build", "-p", "armfortas-rt", "--release"]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cargo_target_directory_honors_absolute_and_relative_overrides() {
        let root = Path::new("/workspace");
        assert_eq!(cargo_target_dir_from(root, None), root.join("target"));
        assert_eq!(
            cargo_target_dir_from(root, Some(OsStr::new("build/cargo"))),
            root.join("build/cargo")
        );
        assert_eq!(
            cargo_target_dir_from(root, Some(OsStr::new("/tmp/armfortas-target"))),
            PathBuf::from("/tmp/armfortas-target")
        );
    }

    #[test]
    fn source_workspace_discovery_requires_armfortas_project_identity() {
        let root = temp_root("workspace-identity");
        let nested = root.join("examples/nested");
        fs::create_dir_all(root.join("src/driver")).unwrap();
        fs::create_dir_all(root.join("runtime/src")).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            b"[workspace]\nmembers = [\"runtime\"]\n\n[package]\nname = \"unrelated\"\n",
        )
        .unwrap();
        fs::write(
            root.join("runtime/Cargo.toml"),
            b"[package]\nname = 'armfortas-rt'\n",
        )
        .unwrap();
        for source in [
            root.join("src/lib.rs"),
            root.join("src/driver/mod.rs"),
            root.join("runtime/src/lib.rs"),
        ] {
            fs::write(source, b"// unrelated fixture\n").unwrap();
        }

        assert!(
            find_source_workspace_from(&[nested]).is_none(),
            "Cargo layout and a coincidental runtime package must not establish compiler ownership"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_workspace_discovery_accepts_the_armfortas_source_tree() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            find_source_workspace_from(&[root.join("src/driver")]),
            Some(root),
            "development callers must still find the owning source workspace"
        );
    }

    #[test]
    fn bundled_runtime_is_private_and_removed_with_its_guard() {
        let archive = b"!<arch>\ncontained runtime bytes";
        let guard = materialize_bundled_runtime(archive).expect("materialize runtime");
        let path = guard.path().to_path_buf();
        let dir = path.parent().expect("runtime has parent").to_path_buf();

        assert_eq!(fs::read(&path).unwrap(), archive);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        drop(guard);
        assert!(!path.exists());
        assert!(!dir.exists());
    }

    #[test]
    fn bundled_runtime_rejects_non_archive_bytes() {
        let err = materialize_bundled_runtime(b"not an archive")
            .err()
            .expect("invalid bytes must fail");
        assert!(err.contains("not a valid archive"), "{err}");
    }
}
