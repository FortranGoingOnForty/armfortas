use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const SUITE_MANIFEST: &str = include_str!("fixtures/bencch_suites.txt");
const NESTED_PROGRAM_MANIFEST: &str = include_str!("fixtures/nested_test_programs.txt");

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn collect_files(root: &Path, extension: Option<&str>, files: &mut Vec<PathBuf>) {
    for entry in
        fs::read_dir(root).unwrap_or_else(|error| panic!("cannot read {}: {error}", root.display()))
    {
        let path = entry.expect("read directory entry").path();
        if path.is_dir() {
            collect_files(&path, extension, files);
        } else if extension.is_none()
            || path.extension().and_then(|value| value.to_str()) == extension
        {
            files.push(path);
        }
    }
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or_else(|_| panic!("{} is outside {}", path.display(), root.display()))
        .to_string_lossy()
        .replace('\\', "/")
}

fn data_lines(manifest: &str) -> impl Iterator<Item = &str> {
    manifest.lines().filter_map(|line| {
        let line = line.trim();
        (!line.is_empty() && !line.starts_with('#')).then_some(line)
    })
}

fn expected_suites() -> BTreeMap<String, (String, usize)> {
    data_lines(SUITE_MANIFEST)
        .map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 3, "malformed suite manifest row: {line}");
            let cases = fields[2]
                .parse::<usize>()
                .unwrap_or_else(|_| panic!("invalid case count in suite manifest row: {line}"));
            assert!(cases > 0, "suite manifest has zero cases: {line}");
            (fields[0].to_owned(), (fields[1].to_owned(), cases))
        })
        .collect()
}

fn observed_suites(root: &Path) -> BTreeMap<String, (String, usize)> {
    let mut files = Vec::new();
    collect_files(&root.join("bencch/suites"), Some("afs"), &mut files);
    files.sort();

    files
        .into_iter()
        .map(|path| {
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
            let names: Vec<_> = source
                .lines()
                .filter_map(|line| {
                    line.trim()
                        .strip_prefix("suite \"")
                        .and_then(|rest| rest.strip_suffix('"'))
                })
                .collect();
            assert_eq!(
                names.len(),
                1,
                "{} must declare exactly one suite name",
                path.display()
            );
            let cases = source
                .lines()
                .filter(|line| line.trim().starts_with("case \""))
                .count();
            assert!(cases > 0, "{} has no authored cases", path.display());
            (relative_path(root, &path), (names[0].to_owned(), cases))
        })
        .collect()
}

fn is_fortran_source(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("f90" | "f95" | "f03" | "f08" | "f18" | "f23" | "f" | "for" | "ftn" | "fpp")
    )
}

fn expected_nested_programs() -> BTreeMap<String, BTreeSet<String>> {
    data_lines(NESTED_PROGRAM_MANIFEST)
        .map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            assert_eq!(
                fields.len(),
                2,
                "malformed nested-program manifest row: {line}"
            );
            let owners = fields[1].split(',').map(str::to_owned).collect();
            (fields[0].to_owned(), owners)
        })
        .collect()
}

fn nested_programs(root: &Path) -> BTreeSet<String> {
    let mut files = Vec::new();
    collect_files(&root.join("test_programs"), None, &mut files);
    files
        .into_iter()
        .filter(|path| is_fortran_source(path))
        .filter_map(|path| {
            let relative = relative_path(root, &path);
            (Path::new(&relative).components().count() > 2).then_some(relative)
        })
        .collect()
}

fn referenced_nested_programs(
    root: &Path,
    nested: &BTreeSet<String>,
) -> BTreeMap<String, BTreeSet<String>> {
    let canonical_root = fs::canonicalize(root).expect("canonicalize workspace root");
    let mut suites = Vec::new();
    collect_files(&root.join("bencch/suites"), Some("afs"), &mut suites);
    let mut owners: BTreeMap<String, BTreeSet<String>> = nested
        .iter()
        .map(|path| (path.clone(), BTreeSet::new()))
        .collect();

    for suite in suites {
        let suite_source = fs::read_to_string(&suite)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", suite.display()));
        let suite_relative = relative_path(root, &suite);
        for line in suite_source.lines() {
            let line = line.trim();
            let referenced = ["source ", "entry ", "file "]
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))
                .and_then(|rest| rest.strip_prefix('"'))
                .and_then(|rest| rest.strip_suffix('"'));
            let Some(referenced) = referenced else {
                continue;
            };
            let resolved = fs::canonicalize(suite.parent().unwrap().join(referenced))
                .unwrap_or_else(|error| {
                    panic!(
                        "{} references unreadable source {}: {error}",
                        suite.display(),
                        referenced
                    )
                });
            let relative = relative_path(&canonical_root, &resolved);
            if let Some(references) = owners.get_mut(&relative) {
                references.insert(suite_relative.clone());
            }
        }
    }
    owners
}

#[test]
fn exact_bencch_suite_inventory_matches_authored_suites() {
    let root = workspace_root();
    let expected = expected_suites();
    let observed = observed_suites(&root);
    assert_eq!(expected.len(), 55, "audit baseline must name all 55 suites");
    assert_eq!(
        expected.values().map(|(_, cases)| cases).sum::<usize>(),
        358,
        "audit baseline must account for all 358 authored cases"
    );
    assert_eq!(observed, expected, "bencch suite identity set changed");
}

#[test]
fn exact_nested_program_inventory_has_bencch_owners() {
    let root = workspace_root();
    let expected = expected_nested_programs();
    let nested = nested_programs(&root);
    assert_eq!(
        nested,
        expected.keys().cloned().collect(),
        "nested test_programs identity set changed"
    );
    assert_eq!(
        referenced_nested_programs(&root, &nested),
        expected,
        "nested test_programs ownership changed"
    );
}
