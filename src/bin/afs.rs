fn main() {
    armfortas::cli_entry_with_bundled_runtime(include_bytes!(env!("ARMFORTAS_BUNDLED_RUNTIME")));
}
