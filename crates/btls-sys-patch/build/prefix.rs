use crate::{config::Config, pick_best_android_ndk_toolchain, run_command};
use std::{fs, io::Write, path::PathBuf, process::Command};

// The prefix to add to all symbols
// Using crate name to avoid collisions with other projects
const PREFIX: &str = env!("CARGO_CRATE_NAME");

// Callback to add a `link_name` macro with the prefix to all generated bindings
#[derive(Debug)]
pub struct PrefixCallback;

impl bindgen::callbacks::ParseCallbacks for PrefixCallback {
    fn generated_link_name_override(
        &self,
        item_info: bindgen::callbacks::ItemInfo<'_>,
    ) -> Option<String> {
        Some(format!("{PREFIX}_{}", item_info.name))
    }
}

fn android_toolchain(config: &Config) -> PathBuf {
    let mut android_bin_path = config
        .env
        .android_ndk_home
        .clone()
        .expect("Please set ANDROID_NDK_HOME for Android build");
    android_bin_path.extend(["toolchains", "llvm", "prebuilt"]);
    android_bin_path.push(pick_best_android_ndk_toolchain(&android_bin_path).unwrap());
    android_bin_path.push("bin");
    android_bin_path
}

pub fn prefix_symbols(config: &Config) {
    // List static libraries to prefix symbols in
    let static_libs: Vec<PathBuf> = [
        config.out_dir.join("build"),
        config.out_dir.join("build").join("ssl"),
        config.out_dir.join("build").join("crypto"),
    ]
    .iter()
    .flat_map(|dir| {
        ["libssl.a", "libcrypto.a"]
            .into_iter()
            .map(move |file| PathBuf::from(dir).join(file))
    })
    .filter(|p| p.exists())
    .collect();

    // Use `nm` to list symbols in these static libraries. Cross builds need
    // the target-prefixed binutils (host objcopy's BFD lacks foreign backends
    // and reports "Unable to recognise the format of the input file").
    let triple_prefix = format!("{}-", config.target);
    let find_bin = |names: &[&str]| -> PathBuf {
        for name in names {
            let candidate = format!("{triple_prefix}{name}");
            if which(&candidate).is_some() {
                return PathBuf::from(candidate);
            }
        }
        names.first().map(PathBuf::from).unwrap()
    };
    let nm = match &*config.target_os {
        "android" => android_toolchain(config).join("llvm-nm"),
        _ => find_bin(&["nm"]),
    };
    let out = run_command(Command::new(nm).args(&static_libs)).unwrap();
    let mut redefine_syms: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| {
            [" T ", " D ", " B ", " C ", " R ", " W "]
                .iter()
                .any(|s| l.contains(s))
        })
        .filter_map(|l| l.split_whitespace().nth(2).map(|s| s.to_string()))
        .filter(|l| !l.starts_with("_"))
        .map(|l| format!("{l} {PREFIX}_{l}"))
        .collect();
    redefine_syms.sort();
    redefine_syms.dedup();

    let redefine_syms_path = config.out_dir.join("redefine_syms.txt");
    let mut f = fs::File::create(&redefine_syms_path).unwrap();
    for sym in &redefine_syms {
        writeln!(f, "{sym}").unwrap();
    }
    f.flush().unwrap();

    // Use `objcopy` to prefix symbols in these static libraries.
    // GNU objcopy applies --redefine-syms to archive members correctly only
    // when invoked per object file: archive-wide invocation is a silent
    // no-op on some binutils builds (observed on ubuntu-22.04 runners with
    // aarch64 archives). Expand, redefine each member, repack.
    let objcopy = match &*config.target_os {
        "android" => android_toolchain(config).join("llvm-objcopy"),
        _ => find_bin(&["objcopy", "llvm-objcopy"]),
    };
    let ar = find_bin(&["ar"]);
    for static_lib in &static_libs {
        let workdir = static_lib.parent().unwrap().join(format!(
            "{}_redefine_work",
            static_lib.file_name().unwrap().to_string_lossy()
        ));
        let _ = fs::remove_dir_all(&workdir);
        fs::create_dir_all(&workdir).unwrap();
        run_command(
            Command::new(&ar)
                .arg("x")
                .arg(static_lib.canonicalize().unwrap())
                .current_dir(&workdir),
        )
        .unwrap();
        for entry in fs::read_dir(&workdir).unwrap().flatten() {
            let object = entry.path();
            if object.extension().is_none() {
                continue;
            }
            run_command(
                Command::new(&objcopy)
                    .arg(format!("--redefine-syms={}", redefine_syms_path.display()))
                    .arg(&object),
            )
            .unwrap();
        }
        let objects: Vec<_> = fs::read_dir(&workdir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        let _ = fs::remove_file(static_lib);
        run_command(
            Command::new(&ar)
                .arg("rcs")
                .arg(static_lib.canonicalize().unwrap())
                .args(&objects)
                .current_dir(&workdir),
        )
        .unwrap();
        let _ = fs::remove_dir_all(&workdir);
    }
}

/// Minimal PATH lookup (avoid a `which` crate dependency).
fn which(name: &str) -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if candidate.metadata().ok()?.permissions().mode() & 0o111 != 0 {
                    return Some(candidate);
                }
            }
            #[cfg(not(unix))]
            return Some(candidate);
        }
    }
    None
}
