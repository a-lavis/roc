use crate::target::arch_str;
use std::io;
use std::path::Path;
use std::process::{Child, Command};
use target_lexicon::{Architecture, OperatingSystem, Triple};

pub fn link(
    target: &Triple,
    binary_path: &Path,
    host_input_path: &Path,
    dest_filename: &Path,
) -> io::Result<Child> {
    match target {
        Triple {
            architecture: Architecture::X86_64,
            operating_system: OperatingSystem::Linux,
            ..
        } => link_linux(target, binary_path, host_input_path, dest_filename),
        Triple {
            architecture: Architecture::X86_64,
            operating_system: OperatingSystem::Darwin,
            ..
        } => link_macos(target, binary_path, host_input_path, dest_filename),
        _ => panic!("TODO gracefully handle unsupported target: {:?}", target),
    }
}

fn link_linux(
    target: &Triple,
    binary_path: &Path,
    host_input_path: &Path,
    dest_filename: &Path,
) -> io::Result<Child> {
    // NOTE: order of arguments to `ld` matters here!
    // The `-l` flags should go after the `.o` arguments
    Command::new("ld")
        .args(&[
            "-arch",
            arch_str(target),
            // Inputs
            dest_filename.to_str().unwrap(),   // app.o
            host_input_path.to_str().unwrap(), // host.o
            // Output
            "-o",
            binary_path.to_str().unwrap(), // app
        ])
        .spawn()
}

fn link_macos(
    target: &Triple,
    binary_path: &Path,
    host_input_path: &Path,
    dest_filename: &Path,
) -> io::Result<Child> {
    // NOTE: order of arguments to `ld` matters here!
    // The `-l` flags should go after the `.o` arguments
    Command::new("ld")
        .args(&[
            "-arch",
            target.architecture.to_string().as_str(),
            // Output
            "-o",
            binary_path.to_str().unwrap(), // app
            // Inputs
            host_input_path.to_str().unwrap(), // host.o
            dest_filename.to_str().unwrap(),   // roc_app.o
            // Libraries - see https://github.com/rtfeldman/roc/pull/554#discussion_r496392274
            // for discussion and further references
            "-lSystem",
            "-lresolv",
            "-lpthread",
            // "-lrt", // TODO shouldn't we need this?
            // "-lc_nonshared", // TODO shouldn't we need this?
            "-lc++", // TODO shouldn't we need this?
                     // "-lgcc", // TODO will eventually need compiler_rt from gcc or something - see https://github.com/rtfeldman/roc/pull/554#discussion_r496370840
                     // "-lunwind", // TODO will eventually need this, see https://github.com/rtfeldman/roc/pull/554#discussion_r496370840
        ])
        .spawn()
}
