/// Return the target-matched static runtime archive built by this package.
///
/// The compiler binaries embed these bytes so a `cargo install` result can
/// link Fortran programs without relying on a separate runtime installation.
pub fn bundled_archive() -> &'static [u8] {
    include_bytes!(env!("ARMFORTAS_BUNDLED_RUNTIME"))
}

/// Return the target-matched shared runtime used by Mach-O outputs.
#[cfg(target_os = "macos")]
pub fn bundled_dylib() -> Option<&'static [u8]> {
    Some(include_bytes!(env!("ARMFORTAS_BUNDLED_RUNTIME_DYLIB")))
}

/// Shared runtime linking is not used on the ELF targets yet.
#[cfg(not(target_os = "macos"))]
pub fn bundled_dylib() -> Option<&'static [u8]> {
    None
}
