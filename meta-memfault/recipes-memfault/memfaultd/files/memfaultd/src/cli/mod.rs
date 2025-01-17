//
// Copyright (c) Memfault, Inc.
// See License.txt for details
use eyre::eyre;
use std::path::Path;
use stderrlog::LogLevelNum;

#[cfg(all(target_os = "linux", feature = "coredump"))]
mod memfault_core_handler;
mod memfaultctl;
mod memfaultd;
mod show_settings;
mod version;

fn init_logger(verbose: LogLevelNum) {
    stderrlog::new()
        .module("memfaultd")
        .verbosity(verbose)
        .init()
        .unwrap();
}

pub fn main() {
    let arg0 = std::env::args().next().unwrap();
    let cmd_name = Path::new(&arg0)
        .file_name()
        .expect("<command name>")
        .to_str()
        .unwrap();

    let result = match cmd_name {
        #[cfg(all(target_os = "linux", feature = "coredump"))]
        "memfault-core-handler" => memfault_core_handler::main(),
        #[cfg(not(all(target_os = "linux", feature = "coredump")))]
        "memfault-core-handler" => Err(eyre!(
            "memfault-core-handler is not supported in this build"
        )),
        "memfaultctl" => memfaultctl::main(),
        "memfaultd" => memfaultd::main(),
        _ => Err(eyre!(
            "Unknown command: {}. Should be memfaultd/memfaultctl/memfault-core-handler.",
            cmd_name
        )),
    };

    match result {
        Ok(_) => (),
        Err(e) => {
            eprintln!("{:#}", e);
            std::process::exit(-1);
        }
    }
}
