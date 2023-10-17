extern crate windres;
use windres::Build;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=manifest/manifest.json");
    Build::new()
        .compile("manifest/Resource.rc")
        .expect("failed to include resources in the dll");
}
