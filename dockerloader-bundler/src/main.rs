use anyhow::{Result, bail};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        bail!("Usage: {} <binary> <output_dir>", args[0]);
    }

    let binary = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);

    fs::create_dir_all(&output)?;

    // Get dependencies using ldd
    let deps = get_dependencies(&binary)?;
    println!("Found {} dependencies", deps.len());

    // Copy the binary
    let binary_dest = output.join("entrypoint");
    fs::copy(&binary, &binary_dest)?;
    println!("Copied binary");

    // Copy all libs to /lib
    let lib_dir = output.join("lib");
    fs::create_dir_all(&lib_dir)?;

    for dep in &deps {
        let filename = dep.file_name().unwrap();
        let dest = lib_dir.join(filename);
        fs::copy(dep, &dest)?;
        println!("  Copied {}", filename.to_string_lossy());
    }

    // Patch rpath to just $ORIGIN/lib
    patch_rpath(&binary_dest)?;

    // Copy SSL certificate bundle
    let cert_dest = output.join("etc/ssl/certs/ca-certificates.crt");
    fs::create_dir_all(cert_dest.parent().unwrap())?;
    fs::copy("/etc/ssl/certs/ca-certificates.crt", &cert_dest)?;
    println!("Copied SSL certificate bundle");

    Ok(())
}

fn get_dependencies(binary: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("ldd")
        .arg(binary)
        .output()?;

    if !output.status.success() {
        bail!("ldd failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut deps = Vec::new();
    let mut seen = HashSet::new();

    for line in stdout.lines() {
        let line = line.trim();

        // Look for " => /path" pattern or lines starting with /
        let path_str = if let Some(idx) = line.find(" => ") {
            let after = &line[idx + 4..];
            after.split_whitespace().next()
        } else if line.starts_with('/') {
            line.split_whitespace().next()
        } else {
            None
        };

        if let Some(path_str) = path_str {
            if path_str.starts_with('/') {
                if let Ok(canonical) = fs::canonicalize(path_str) {
                    if seen.insert(canonical.clone()) {
                        deps.push(canonical);
                    }
                }
            }
        }
    }

    Ok(deps)
}

fn patch_rpath(binary: &Path) -> Result<()> {
    Command::new("patchelf")
        .arg("--set-rpath")
        .arg("$ORIGIN/lib")
        .arg(binary)
        .status()?;

    println!("Patched rpath");
    Ok(())
}
