use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::{env, fs};

use cargo_metadata::{
    Artifact, CompilerMessage, Message, Metadata, MetadataCommand, Package, Target,
};

const RECOMPILE_OPT: &str = "RAK_XDP_EBPF_COMPILE";
const TRACE_OPT: &str = "RAK_XDP_EBFP_TRACE";

fn main() {
    println!("cargo:rerun-if-env-changed={}", RECOMPILE_OPT);
    println!("cargo:rerun-if-env-changed={}", TRACE_OPT);

    let enable_trace = env::var(TRACE_OPT).unwrap_or_default();
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let out_dir = PathBuf::from(out_dir);
    let endian = env::var_os("CARGO_CFG_TARGET_ENDIAN").unwrap();
    let target = if endian == "big" {
        "bpfeb-unknown-none"
    } else if endian == "little" {
        "bpfel-unknown-none"
    } else {
        panic!("unsupported endian={:?}", endian)
    };
    let Metadata { packages, .. } = MetadataCommand::new().no_deps().exec().unwrap();
    let ebpf_package = packages
        .into_iter()
        .find(|Package { name, .. }| name == "ebpf")
        .unwrap();
    let Package { manifest_path, .. } = ebpf_package;
    let ebpf_dir = manifest_path.parent().unwrap();

    println!("cargo:rerun-if-changed={}", ebpf_dir.as_str());

    // build all bins in ebpf
    let mut cmd = Command::new("cargo");
    cmd.current_dir(ebpf_dir);
    cmd.args([
        "build",
        "-Z",
        "build-std=core",
        "--bins",
        "--message-format=json",
        "--release",
        "--target",
        target,
    ]);
    if matches!(enable_trace.to_lowercase().as_str(), "1" | "on" | "true") {
        cmd.args(["--features", "trace"]);
    }

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("failed to spawn {cmd:?}: {err}"));
    let Child { stdout, stderr, .. } = &mut child;

    // Trampoline stdout to cargo warnings.
    let stderr = BufReader::new(stderr.take().unwrap());
    let stderr = std::thread::spawn(move || {
        for line in stderr.lines() {
            let line = line.unwrap();
            println!("cargo:warning={line}");
        }
    });

    let stdout = BufReader::new(stdout.take().unwrap());
    let mut executables = Vec::new();
    for message in Message::parse_stream(stdout) {
        #[allow(clippy::collapsible_match)]
        match message.expect("valid JSON") {
            Message::CompilerArtifact(Artifact {
                executable,
                target: Target { name, .. },
                ..
            }) => {
                if let Some(executable) = executable {
                    executables.push((name, executable.into_std_path_buf()));
                }
            }
            Message::CompilerMessage(CompilerMessage { message: msg, .. }) => {
                for line in msg.rendered.unwrap_or_default().split('\n') {
                    println!("cargo:warning={line}");
                }
            }
            Message::TextLine(line) => {
                println!("cargo:warning={line}");
            }
            _ => {}
        }
    }

    let status = child
        .wait()
        .unwrap_or_else(|err| panic!("failed to wait for {cmd:?}: {err}"));
    assert_eq!(status.code(), Some(0), "{cmd:?} failed: {status:?}");

    stderr.join().map_err(std::panic::resume_unwind).unwrap();

    // copy to $OUT_DIR
    for (name, binary) in executables {
        let dst = out_dir.join(name);
        let _: u64 = fs::copy(&binary, &dst)
            .unwrap_or_else(|err| panic!("failed to copy {binary:?} to {dst:?}: {err}"));
    }

    make_binding();
}

fn make_binding() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let bindings = bindgen::Builder::default()
        .header(root.join("bindings/input.h").display().to_string())
        .allowlist_var("ETHTOOL_GCHANNELS")
        .allowlist_type("ethtool_channels")
        .rust_target(bindgen::RustTarget::Stable_1_47)
        .layout_tests(false)
        .raw_line(
            r#"
#![allow(non_camel_case_types)]
            "#
            .trim(),
        )
        .generate()
        .expect("gen binding failed");

    let out = root.join("src/bindings.rs");
    bindings
        .write_to_file(out)
        .expect("write binding.rs failed");
}
