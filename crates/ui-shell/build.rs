use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=windows-resource.rc");
    println!("cargo:rerun-if-changed=../../assets/icons/iris-icon.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("windows-resource.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("failed to compile Windows resources");
    }
}
