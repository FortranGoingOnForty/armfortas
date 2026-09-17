# Releasing ARMFORTAS

ARMFORTAS releases use a complete source asset because GitHub's generated
source archives do not include the `afs-as`, `afs-ld`, or `bencch` submodules.
Downstream packages must use the attached `armfortas-VERSION.tar.gz` asset and
its published SHA-256 checksum.

## Release scope

- Homebrew: Apple Silicon macOS only.
- AUR: Arch Linux x86_64 only.
- Other supported targets remain source builds until they have a native
  package and a clean-install gate.
- 0.1.x releases are marked as GitHub prereleases while the compiler remains
  an active preview.

## Before tagging

1. Confirm `trunk` is clean and the full CI matrix is green.
2. Confirm every submodule is initialized at its pinned commit and that those
   commits are available from the public HTTPS remotes.
3. Keep the `armfortas`, `armfortas-rt`, `afs-as`, and `afs-ld` versions in
   sync. The tag must be `vVERSION`, matching the root `Cargo.toml` version.
4. Run the release packaging regressions:

   ```bash
   cargo test --test release_packaging --test release_workflow
   ```

5. Build and validate the complete source asset locally:

   ```bash
   scripts/package-release-source.sh VERSION dist
   scripts/validate-release-source.sh dist/armfortas-VERSION.tar.gz
   ```

6. Open the release-hardening pull request and require its Release workflow to
   pass on macOS ARM64, Ubuntu x86_64, and Arch Linux x86_64.

## Tag and GitHub release

Create an annotated tag on the fully green merge commit and push only that
tag:

```bash
git tag -a vVERSION -m "ARMFORTAS VERSION"
git push origin vVERSION
```

The tag workflow rebuilds the complete source archive, verifies its checksum,
installs it in empty prefixes on all three release runners, compiles and runs a
native Fortran smoke program, and only then creates the prerelease. A failed
validation job must leave the tag without a GitHub release.

After publication, download the attached archive and verify the published
checksum independently before updating downstream package hashes.

## Homebrew

Update `Formula/armfortas.rb` in `homebrew-tap` to the release-asset URL and
SHA-256. The formula must require macOS ARM64 and Rust at build time, install
through `cargo install`, verify both command names, and compile and run a small
Fortran program in its test block.

Run at least:

```bash
brew style Formula/armfortas.rb
brew audit --strict Formula/armfortas.rb
brew install --build-from-source Formula/armfortas.rb
brew test armfortas
```

## AUR

Update the `armfortas` `PKGBUILD` and `.SRCINFO` from the same release asset.
The package architecture is `x86_64`; Cargo is a build dependency, while GCC
and binutils are runtime requirements because the compiler directly invokes
the system linker and consumes the installed GCC crt objects.

Build in a clean Arch environment, run `namcap` on the package when available,
install it, verify both command names, and compile and run a small Fortran
program before pushing the AUR commit.
