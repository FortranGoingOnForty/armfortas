const WORKFLOW: &str = include_str!("../.github/workflows/release.yml");

#[test]
fn release_workflow_validates_complete_sources_before_publishing() {
    for required in [
        "pull_request:",
        "tags:\n      - \"v*\"",
        "scripts/package-release-source.sh",
        "package_id=${package_id##*#}\n            version=${package_id##*@}",
        "submodules: recursive",
        "actions/upload-artifact@v7",
        "actions/download-artifact@v7",
        "- os: macos-15\n            platform: macOS ARM64",
        "- os: ubuntu-latest\n            platform: Linux x86_64 glibc",
        "container: archlinux:base-devel",
        "scripts/validate-release-source.sh",
        "- validate-source",
        "- validate-arch",
        "gh release create",
        "--verify-tag",
        "--prerelease",
    ] {
        assert!(
            WORKFLOW.contains(required),
            "release workflow is missing required control: {required:?}"
        );
    }
}

#[test]
fn pull_requests_cannot_publish_a_release() {
    assert!(WORKFLOW.contains("if: startsWith(github.ref, 'refs/tags/v')"));
    let publish = WORKFLOW
        .split("\n  publish:\n")
        .nth(1)
        .expect("release workflow has a publish job");
    assert!(publish.contains("contents: write"));
    assert!(publish
        .contains("needs:\n      - source-bundle\n      - validate-source\n      - validate-arch"));
}
