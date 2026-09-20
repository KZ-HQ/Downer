//! Derive compile-time constants from files the extension also reads.
//!
//! The Firefox add-on ID lives in `extension/manifest.json`, because that is
//! the file Firefox itself reads. The native messaging host has to name the
//! same ID in `allowed_extensions`, and a mismatch makes Firefox refuse the
//! connection with a message that names neither side. Rather than write the ID
//! twice and test that the two copies agree, the build reads the manifest and
//! hands `src/host.rs` the value, so drift is not possible (KEI-58).

use std::{env, fs, path::Path};

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_path = root.join("extension").join("manifest.json");

    // Cargo caches build scripts, so without this a manifest edit would leave a
    // stale ID compiled into the host until something else forced a rebuild.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", manifest_path.display());

    let text = fs::read_to_string(&manifest_path).unwrap_or_else(|error| {
        panic!("cannot read {}: {error}", manifest_path.display());
    });
    let manifest: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!("{} is not valid JSON: {error}", manifest_path.display());
    });

    let id = manifest["browser_specific_settings"]["gecko"]["id"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "{} has no browser_specific_settings.gecko.id string",
                manifest_path.display()
            );
        });
    // A newline would end the directive and silently truncate the value.
    assert!(
        !id.is_empty() && !id.contains('\n'),
        "the extension ID in {} must be a non-empty single line, got {id:?}",
        manifest_path.display()
    );

    println!("cargo:rustc-env=DOWNER_EXTENSION_ID={id}");
}
