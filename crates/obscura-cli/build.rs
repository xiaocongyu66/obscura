use std::env;

fn env_value(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_version(version: String) -> String {
    version.strip_prefix('v').unwrap_or(&version).to_string()
}

fn github_tag_version() -> Option<String> {
    if env_value("GITHUB_REF_TYPE").as_deref() == Some("tag") {
        return env_value("GITHUB_REF_NAME").map(normalize_version);
    }

    env_value("GITHUB_REF")
        .and_then(|value| value.strip_prefix("refs/tags/").map(str::to_owned))
        .map(normalize_version)
}

fn main() {
    println!("cargo:rerun-if-env-changed=OBSCURA_VERSION");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_TYPE");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_NAME");
    println!("cargo:rerun-if-env-changed=GITHUB_REF");

    let version = env_value("OBSCURA_VERSION")
        .map(normalize_version)
        .or_else(github_tag_version)
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").expect("Cargo sets CARGO_PKG_VERSION"));

    println!("cargo:rustc-env=OBSCURA_BUILD_VERSION={version}");

    link_boringssl_statics();
}

/// btls-sys 的 `cargo:rustc-link-lib=static=crypto/ssl` 在交叉构建(zigbuild)
/// 下有时不展开进最终二进制的链接命令行(-L 传了、-l 没传),导致
/// `build_script_main_SSL_read` 等符号未定义。这里直接按绝对路径把
/// libcrypto.a/libssl.a 塞进链接参数,绕过 rlib metadata 的传递环节。
fn link_boringssl_statics() {
    println!("cargo:rerun-if-env-changed=OBSCURA_LINK_BORINGSSL_EXPLICIT");
    if env_value("OBSCURA_LINK_BORINGSSL_EXPLICIT").is_none() {
        return;
    }
    let target_dir = env::var("CARGO_TARGET_DIR")
        .map(|v| v.into())
        .unwrap_or_else(|_| "target".to_string());
    let target = env::var("TARGET").unwrap_or_default();
    // build.rs 的 cwd = crate 目录(crates/obscura-cli);workspace 根在其上两级
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| env::current_dir().expect("cwd"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| manifest_dir.clone());
    let build_root = if workspace_root.join(&target_dir).exists() {
        workspace_root.join(&target_dir)
    } else {
        workspace_root
    };
    let glob = build_root
        .join(&target)
        .join("release")
        .join("build");
    println!("cargo:warning=link_boringssl_statics: glob={}", glob.display());
    let mut found: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&glob) {
        for entry in entries.flatten() {
            let out = entry.path().join("out");
            if !entry.file_name().to_string_lossy().contains("btls-sys") || !out.exists() {
                continue;
            }
            for sub in ["build/lib", "build/ssl", "build/crypto", "build", "lib"] {
                for lib in ["libssl.a", "libcrypto.a"] {
                    let p = out.join(sub).join(lib);
                    if p.exists() && !found.contains(&p) {
                        found.push(p);
                    }
                }
            }
        }
    }
    // 链接顺序:libssl.a 依赖 libcrypto.a,ssl 在前
    found.sort_by_key(|p| if p.file_name().unwrap() == "libssl.a" { 0 } else { 1 });
    for lib in found {
        println!("cargo:warning=linking {}", lib.display());
        println!("cargo:rustc-link-arg={}", lib.display());
    }
}
