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
        command: "cargo test -p afs-as --all-targets -- --nocapture",
    },
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

fn missing_required_suites(workflow: &str) -> Vec<&'static str> {
    let commands = workflow_run_commands(workflow);
    REQUIRED_SUITES
        .iter()
        .filter(|suite| !commands.contains(&(suite.job, suite.command)))
        .map(|suite| suite.name)
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
