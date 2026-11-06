use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn source_files(path: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(path).expect("API source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            source_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=src");
    // NativeSettings is a private wire value re-exported from the pure domain
    // crate. Its fields, defaults and enum encodings are part of compatibility.
    println!("cargo:rerun-if-changed=../bazza-domain/src");
    println!("cargo:rerun-if-changed=build.rs");
    let mut paths = Vec::new();
    source_files(Path::new("src"), &mut paths);
    source_files(Path::new("../bazza-domain/src"), &mut paths);
    paths.sort();
    let mut hash = Sha256::new();
    hash.update(env!("CARGO_PKG_VERSION").as_bytes());
    for path in paths {
        hash.update(path.to_string_lossy().replace('\\', "/").as_bytes());
        hash.update([0]);
        hash.update(fs::read(path).expect("API source"));
        hash.update([0]);
    }
    println!("cargo:rustc-env=BAZZA_API_BUILD_ID={:x}", hash.finalize());
}
