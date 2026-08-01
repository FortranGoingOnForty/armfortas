use std::path::Path;

const WORKFLOW: &str = include_str!("../.github/workflows/ci.yml");

#[derive(Clone, Copy, Debug)]
struct RequiredSuite {
    job: &'static str,
    name: &'static str,
    command: &'static str,
}

const REQUIRED_SUITES: &[RequiredSuite] = &[
    RequiredSuite {
        job: "test-workspace-libs",
        name: "workspace library unit suites",
        command: "cargo test --workspace --lib --release",
    },
    RequiredSuite {
        job: "test-afs-as",
        name: "complete afs-as suite",
        command: "ci/run_logged.sh /tmp/afs-as-tests.log cargo test -p afs-as --all-targets -- --nocapture",
    },
    RequiredSuite {
        job: "test-afs-ld",
        name: "complete afs-ld suite with serialized skip evidence",
        command: "ci/run_logged.sh /tmp/afs-ld-tests.log cargo test -p afs-ld --all-targets -- --nocapture --test-threads=1",
    },
];

const AFS_AS_REQUIRED_CONTROLS: &[(&str, &str)] = &[
    (
        "pinned Apple-arm64 runner",
        "- os: macos-15\nhost_os: Darwin\nhost_arch: arm64\nskip_profile: macos-arm64",
    ),
    (
        "pinned Linux-x86_64 runner",
        "- os: ubuntu-latest\nhost_os: Linux\nhost_arch: x86_64\nskip_profile: linux-x86_64",
    ),
    (
        "runtime host assertion",
        r#"ci/assert_host.sh "${{ matrix.host_os }}" "${{ matrix.host_arch }}""#,
    ),
    (
        "assembler platform-skip gate",
        r#"ci/check_afs_as_skips.sh /tmp/afs-as-tests.log "${{ matrix.skip_profile }}""#,
    ),
];

const AFS_LD_REQUIRED_INPUTS: &[(&str, &str)] = &[
    ("afs-ld submodule pointer", "afs-ld"),
    ("afs-ld source paths", "afs-ld/**"),
    ("afs-as submodule pointer", "afs-as"),
    ("afs-as source paths", "afs-as/**"),
    ("runtime package", "runtime/**"),
    ("workspace manifest", "Cargo.toml"),
    ("workspace lockfile", "Cargo.lock"),
    ("workflow definition", ".github/workflows/ci.yml"),
];

const AFS_LD_REQUIRED_CONTROLS: &[(&str, &str)] = &[
    (
        "macOS skip-accounting profile",
        "- os: macos-14\nskip_profile: macos",
    ),
    (
        "Linux skip-accounting profile",
        "- os: ubuntu-latest\nskip_profile: linux",
    ),
    (
        "linker prerequisite-skip gate",
        r#"afs-ld/ci/check_skips.sh /tmp/afs-ld-tests.log "${{ matrix.skip_profile }}""#,
    ),
];

fn workflow_run_commands(workflow: &str) -> Vec<(&str, &str)> {
    let mut current_job = None;
    let mut commands = Vec::new();

    for line in workflow.lines() {
        let trimmed = line.trim();
        let indentation = line.len() - line.trim_start().len();
        if indentation == 2 && trimmed.ends_with(':') {
            current_job = Some(trimmed.trim_end_matches(':'));
        } else if let (Some(job), Some(command)) = (current_job, trimmed.strip_prefix("- run:")) {
            commands.push((job, command.trim()));
        }
    }

    commands
}

fn workflow_job(workflow: &str, required_job: &str) -> String {
    let mut current_job = None;
    let mut lines = Vec::new();

    for line in workflow.lines() {
        let trimmed = line.trim();
        let indentation = line.len() - line.trim_start().len();
        if indentation == 2 && trimmed.ends_with(':') {
            current_job = Some(trimmed.trim_end_matches(':'));
        } else if current_job == Some(required_job) {
            lines.push(trimmed);
        }
    }

    lines.join("\n")
}

fn missing_required_suites(workflow: &str) -> Vec<&'static str> {
    let commands = workflow_run_commands(workflow);
    REQUIRED_SUITES
        .iter()
        .filter(|suite| !commands.contains(&(suite.job, suite.command)))
        .map(|suite| suite.name)
        .collect()
}

fn missing_afs_as_controls(workflow: &str) -> Vec<&'static str> {
    let job = workflow_job(workflow, "test-afs-as");
    AFS_AS_REQUIRED_CONTROLS
        .iter()
        .filter(|(_, required)| !job.contains(required))
        .map(|(name, _)| *name)
        .collect()
}

fn missing_afs_ld_controls(workflow: &str) -> Vec<&'static str> {
    let job = workflow_job(workflow, "test-afs-ld");
    AFS_LD_REQUIRED_CONTROLS
        .iter()
        .filter(|(_, required)| !job.contains(required))
        .map(|(name, _)| *name)
        .collect()
}

fn workflow_filter_paths<'a>(workflow: &'a str, required_filter: &str) -> Vec<&'a str> {
    let mut current_filter = None;
    let mut paths = Vec::new();

    for line in workflow.lines() {
        let trimmed = line.trim();
        let indentation = line.len() - line.trim_start().len();
        if indentation == 12 && trimmed.ends_with(':') {
            current_filter = Some(trimmed.trim_end_matches(':'));
        } else if current_filter == Some(required_filter) && indentation == 14 {
            if let Some(path) = trimmed
                .strip_prefix("- '")
                .and_then(|path| path.strip_suffix('\''))
            {
                paths.push(path);
            }
        }
    }

    paths
}

fn missing_afs_ld_inputs(workflow: &str) -> Vec<&'static str> {
    let paths = workflow_filter_paths(workflow, "afs_ld");
    AFS_LD_REQUIRED_INPUTS
        .iter()
        .filter(|(_, required)| !paths.contains(required))
        .map(|(name, _)| *name)
        .collect()
}

#[test]
fn required_ci_executes_complete_owned_suites() {
    let missing = missing_required_suites(WORKFLOW);
    assert!(
        missing.is_empty(),
        "{} does not execute required suites: {}",
        Path::new(".github/workflows/ci.yml").display(),
        missing.join(", ")
    );
}

#[test]
fn policy_check_rejects_each_omitted_suite() {
    let complete = format!(
        "jobs:\n{}",
        REQUIRED_SUITES
            .iter()
            .map(|suite| format!(
                "  {}:\n    steps:\n      - run: {}\n",
                suite.job, suite.command
            ))
            .collect::<String>()
    );
    assert!(missing_required_suites(&complete).is_empty());

    for suite in REQUIRED_SUITES {
        let incomplete = complete.replacen(&format!("      - run: {}\n", suite.command), "", 1);
        assert_eq!(missing_required_suites(&incomplete), vec![suite.name]);
    }
}

#[test]
fn afs_as_ci_exposes_and_rejects_platform_skips() {
    let missing = missing_afs_as_controls(WORKFLOW);
    assert!(
        missing.is_empty(),
        "{} does not enforce afs-as platform coverage: {}",
        Path::new(".github/workflows/ci.yml").display(),
        missing.join(", ")
    );
}

#[test]
fn policy_check_rejects_each_missing_afs_as_control() {
    let complete = format!(
        "jobs:\n  test-afs-as:\n    {}\n",
        AFS_AS_REQUIRED_CONTROLS
            .iter()
            .map(|(_, required)| required.replace('\n', "\n    "))
            .collect::<Vec<_>>()
            .join("\n    ")
    );
    assert!(missing_afs_as_controls(&complete).is_empty());

    for (name, required) in AFS_AS_REQUIRED_CONTROLS {
        let rendered = required.replace('\n', "\n    ");
        let incomplete = complete.replacen(&rendered, "", 1);
        assert_eq!(missing_afs_as_controls(&incomplete), vec![*name]);
    }
}

#[test]
fn afs_ld_ci_exposes_and_rejects_false_green_skips() {
    let missing = missing_afs_ld_controls(WORKFLOW);
    assert!(
        missing.is_empty(),
        "{} does not enforce afs-ld skip accounting: {}",
        Path::new(".github/workflows/ci.yml").display(),
        missing.join(", ")
    );
}

#[test]
fn policy_check_rejects_each_missing_afs_ld_control() {
    let complete = format!(
        "jobs:\n  test-afs-ld:\n    {}\n",
        AFS_LD_REQUIRED_CONTROLS
            .iter()
            .map(|(_, required)| required.replace('\n', "\n    "))
            .collect::<Vec<_>>()
            .join("\n    ")
    );
    assert!(missing_afs_ld_controls(&complete).is_empty());

    for (name, required) in AFS_LD_REQUIRED_CONTROLS {
        let rendered = required.replace('\n', "\n    ");
        let incomplete = complete.replacen(&rendered, "", 1);
        assert_eq!(missing_afs_ld_controls(&incomplete), vec![*name]);
    }
}

#[test]
fn afs_ld_ci_tracks_every_built_or_consumed_input() {
    let missing = missing_afs_ld_inputs(WORKFLOW);
    assert!(
        missing.is_empty(),
        "{} does not rerun afs-ld compatibility tests for changes to: {}",
        Path::new(".github/workflows/ci.yml").display(),
        missing.join(", ")
    );
}

#[test]
fn policy_check_rejects_each_missing_afs_ld_input() {
    let complete = format!(
        "jobs:\n  changes:\n    steps:\n      - with:\n          filters: |\n            afs_ld:\n{}\n",
        AFS_LD_REQUIRED_INPUTS
            .iter()
            .map(|(_, path)| format!("              - '{path}'"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(missing_afs_ld_inputs(&complete).is_empty());

    for (name, path) in AFS_LD_REQUIRED_INPUTS {
        let incomplete = complete.replacen(&format!("              - '{path}'\n"), "", 1);
        assert_eq!(missing_afs_ld_inputs(&incomplete), vec![*name]);
    }
}
