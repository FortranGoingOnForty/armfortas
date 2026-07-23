/// Return the target-matched static runtime archive built by this package.
///
/// The compiler binaries embed these bytes so a `cargo install` result can
/// link Fortran programs without relying on a separate runtime installation.
pub fn bundled_archive() -> &'static [u8] {
    include_bytes!(env!("ARMFORTAS_BUNDLED_RUNTIME"))
}
