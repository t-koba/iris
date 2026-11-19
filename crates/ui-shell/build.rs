use std::env;

include!("build/version_info.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build/version_info.rs");
    println!("cargo:rerun-if-changed=windows-resource.rc");
    println!("cargo:rerun-if-changed=../../assets/icons/iris-icon.ico");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let version = env::var("CARGO_PKG_VERSION")
            .expect("CARGO_PKG_VERSION is required for Windows VERSIONINFO");
        let numeric_version = windows_numeric_version(&version)
            .unwrap_or_else(|error| panic!("invalid CARGO_PKG_VERSION {version:?}: {error}"));

        let manifest_dir = PathBuf::from(
            env::var_os("CARGO_MANIFEST_DIR")
                .expect("CARGO_MANIFEST_DIR is required to locate the Windows icon"),
        );
        let icon_path = normalize_icon_path(&manifest_dir.join("../../assets/icons/iris-icon.ico"));
        let icon_literal = icon_path.to_string_lossy().replace('\\', "/");

        let template = std::fs::read_to_string("windows-resource.rc")
            .expect("failed to read windows-resource.rc template");
        let rendered = render_version_info(&template, &icon_literal, &numeric_version, &version)
            .unwrap_or_else(|error| panic!("failed to render Windows resource template: {error}"));

        let out_dir = env::var_os("OUT_DIR").expect("OUT_DIR is required for generated resources");
        let generated = PathBuf::from(out_dir).join("iris-version-info.rc");
        std::fs::write(&generated, rendered).expect("failed to write generated Windows resource");

        embed_resource::compile(&generated, embed_resource::NONE)
            .manifest_optional()
            .expect("failed to compile Windows resources");
    }
}
