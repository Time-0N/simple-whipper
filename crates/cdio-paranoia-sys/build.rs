//! Locates libcdio / libcdio-paranoia with pkg-config, emits the linker
//! flags, and runs bindgen over `wrapper.h`.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");

    // Each probe prints `cargo:rustc-link-lib=...` / `-search=...` for us.
    let mut include_paths = Vec::new();
    for lib in ["libcdio", "libcdio_cdda", "libcdio_paranoia"] {
        let found = pkg_config::Config::new()
            .probe(lib)
            .unwrap_or_else(|e| panic!("pkg-config could not find {lib}: {e}"));
        include_paths.extend(found.include_paths);
    }

    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        // Only the pieces we actually use; keeps the bindings small.
        .allowlist_function("cdio_cddap_.*")
        .allowlist_function("cdio_paranoia_.*")
        .allowlist_function("cdio_get_devices")
        .allowlist_function("cdio_free_device_list")
        .allowlist_type("cdrom_drive_t")
        .allowlist_type("cdrom_paranoia_t")
        .allowlist_type("paranoia_mode_t")
        .allowlist_type("paranoia_cb_mode_t")
        .allowlist_type("driver_id_t")
        .allowlist_var("CDIO_CD_FRAMESIZE_RAW")
        .allowlist_var("CDDA_MESSAGE_.*")
        // C enums become Rust modules of consts: no UB on unknown values.
        .constified_enum_module("paranoia_mode_t")
        .constified_enum_module("paranoia_cb_mode_t")
        .constified_enum_module("driver_id_t")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    for path in &include_paths {
        builder = builder.clang_arg(format!("-I{}", path.display()));
    }

    let bindings = builder
        .generate()
        .expect("bindgen failed to generate libcdio-paranoia bindings");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings.rs");
    bindings
        .write_to_file(out)
        .expect("could not write bindings.rs");
}
