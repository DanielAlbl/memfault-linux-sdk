//
// Copyright (c) Memfault, Inc.
// See License.txt for details
use std::collections::HashMap;
use std::env;
use std::fs::write;
use std::path::{Path, PathBuf};

/// Generates $OUT_DIR/build_info.rs, based on the values in the VERSION file (generated by sdk_release.py).
fn generate_build_info_rs() {
    let mut version_file_path = PathBuf::from(&env::var("CARGO_MANIFEST_DIR").unwrap());
    version_file_path.push("../VERSION");
    println!("cargo:rerun-if-changed={}", version_file_path.display());

    let version_file_content =
        std::fs::read_to_string(version_file_path).unwrap_or_else(|_| String::new());

    let key_values_from_version_file: HashMap<&str, &str> = version_file_content
        .lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();

    struct VersionVarInfo {
        key: &'static str,
        var: &'static str,
        default: &'static str,
    }

    let keys_and_vars_defaults = [
        VersionVarInfo {
            key: "VERSION",
            var: "VERSION",
            default: "dev",
        },
        VersionVarInfo {
            key: "GIT COMMIT",
            var: "GIT_COMMIT",
            default: "unknown",
        },
        VersionVarInfo {
            key: "BUILD ID",
            var: "BUILD_ID",
            default: "unknown",
        },
    ];

    let build_info_rs_src = keys_and_vars_defaults
        .iter()
        .fold(String::new(), |mut acc, info| {
            let value = key_values_from_version_file
                .get(info.key)
                .unwrap_or(&info.default);
            acc.push_str(&format!("pub const {}: &str = \"{}\";\n", info.var, value));
            acc
        });

    let out_dir = env::var_os("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("build_info.rs");
    write(&dest_path, build_info_rs_src).unwrap();
}

fn main() {
    generate_build_info_rs();
    println!("cargo:rerun-if-changed=build.rs");
}
