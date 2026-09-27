use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is required"));
    println!("cargo:rerun-if-changed=icons/macos-tray.png");
    let catalog_path =
        manifest_dir.join("../../shared/catalog-floor.json");
    println!("cargo:rerun-if-changed={}", catalog_path.display());
    let catalog_raw = fs::read_to_string(&catalog_path).unwrap_or_else(|error| {
        panic!(
            "read packaged catalog rollback floor {}: {error}",
            catalog_path.display()
        )
    });
    let catalog: serde_json::Value = serde_json::from_str(&catalog_raw).unwrap_or_else(|error| {
        panic!(
            "parse packaged catalog rollback floor {}: {error}",
            catalog_path.display()
        )
    });
    let sequence = catalog
        .get("sequence")
        .and_then(serde_json::Value::as_i64)
        .filter(|sequence| *sequence > 0)
        .unwrap_or_else(|| {
            panic!(
                "packaged catalog {} must contain a positive integer sequence",
                catalog_path.display()
            )
        });
    println!("cargo:rustc-env=CONST_API_PACKAGED_CATALOG_SEQUENCE={sequence}");
    for (field, environment) in [
        ("release_id", "CONST_API_PACKAGED_CATALOG_RELEASE_ID"),
        ("model_version_id", "CONST_API_PACKAGED_MODEL_VERSION_ID"),
        (
            "compatibility_version_id",
            "CONST_API_PACKAGED_COMPATIBILITY_VERSION_ID",
        ),
        (
            "pricing_version_id",
            "CONST_API_PACKAGED_PRICING_VERSION_ID",
        ),
    ] {
        let value = catalog
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                panic!(
                    "packaged catalog {} must contain a non-empty {field}",
                    catalog_path.display()
                )
            });
        println!("cargo:rustc-env={environment}={value}");
    }

    if std::env::var("PROFILE").as_deref() == Ok("release") && tauri_build::is_dev() {
        panic!(
            "refusing to build a release executable with Tauri's development protocol; \
             run `npm run build` from client/ so the frontend is built and \
             the renderer is embedded"
        );
    }
    tauri_build::build()
}
