use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
}
