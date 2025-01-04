use std::env;
use std::path::PathBuf;

fn main() {
    // Link against opencbm
    println!("cargo:rustc-link-lib=opencbm");

    // Regenerate bindings if the wrapper changes
    println!("cargo:rerun-if-changed=wrapper.h");

    // Create the bindings using bindgen
    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .generate()
        .expect("Unable to generate bindings");

    // Write the bindings to a file in OUT_DIR
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}
