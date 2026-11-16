#![allow(dead_code)] // each test crate uses a different subset

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn archgraph(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_boundrail"))
        .args(args)
        .output()
        .unwrap()
}

/// Writes `contents` to a file under the test temp dir and returns its path.
pub fn write_tmp(name: &str, contents: &str) -> String {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, contents).unwrap();
    path.to_str().unwrap().to_owned()
}
