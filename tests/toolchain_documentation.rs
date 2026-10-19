const README: &str = include_str!("../README.md");

#[test]
fn assembler_routing_documentation_matches_the_driver_contract() {
    for required in [
        "Native `arm64-macos` builds invoke\nApple's `as` by default.",
        "x86_64 ELF builds use `afs-as` in-process by default",
        "cross-target Mach-O object builds use its embedded ARM64 path",
        "`AFS_AS_PATH` to select an external assembler explicitly",
        "`AFS_AS=0` selects the system `as`",
    ] {
        assert!(
            README.contains(required),
            "README is missing assembler-routing contract: {required:?}"
        );
    }
    assert!(
        !README.contains("the default build never calls the system `as`"),
        "README restored the false all-target assembler claim"
    );
}
