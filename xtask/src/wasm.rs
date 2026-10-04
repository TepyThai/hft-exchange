//! `cargo xtask wasm [--out PATH]`: build the engine for the browser.
//!
//! Compiles `crates/wasm` for `wasm32-unknown-unknown` with the `wasm`
//! profile, shrinks it with `wasm-opt` when that is installed, and copies it
//! to `target/wasm/hft-engine.wasm` (or `--out`). The tepy.dev live page
//! loads it from `/wasm/hft-engine.wasm`.

use crate::Args;
use std::path::PathBuf;
use std::process::Command;

pub fn cmd_wasm(args: &Args) -> Result<bool, String> {
    let root = crate::report::workspace_root();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(&cargo)
        .current_dir(&root)
        .args(["build", "-p", "engine-wasm", "--lib", "--target", "wasm32-unknown-unknown", "--profile", "wasm"])
        .status()
        .map_err(|e| format!("cargo: {e}"))?;
    if !status.success() {
        return Err("build failed (is the target installed? `rustup target add wasm32-unknown-unknown`)".into());
    }
    let built = root.join("target/wasm32-unknown-unknown/wasm/engine_wasm.wasm");
    let out = args.value("out").map(PathBuf::from).unwrap_or_else(|| root.join("target/wasm/hft-engine.wasm"));
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let optimized = Command::new("wasm-opt")
        .args(["-O3", "--enable-bulk-memory", "--enable-nontrapping-float-to-int", "--enable-sign-ext", "--enable-mutable-globals"])
        .arg(&built)
        .arg("-o")
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    if !optimized {
        std::fs::copy(&built, &out).map_err(|e| format!("copy to {}: {e}", out.display()))?;
    }
    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    println!(
        "wrote {} ({:.0} KiB{})",
        out.display(),
        size as f64 / 1024.0,
        if optimized { ", wasm-opt -O3" } else { "; install binaryen's wasm-opt to shrink it" }
    );
    Ok(true)
}
