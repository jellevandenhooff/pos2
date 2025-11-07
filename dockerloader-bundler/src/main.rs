use anyhow::{Context, Result, bail};
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

    let mut copied_filenames = HashSet::new();

    for dep in &deps {
        let filename = dep.file_name().unwrap();

        // Skip if we already copied this filename
        if !copied_filenames.insert(filename.to_os_string()) {
            continue;
        }

        let dest = lib_dir.join(filename);

        let metadata = fs::symlink_metadata(dep)
            .with_context(|| format!("failed to get metadata for {}", dep.display()))?;

        if metadata.is_symlink() {
            let target = fs::read_link(dep)?;
            std::os::unix::fs::symlink(&target, &dest)?;
            println!("  Copied symlink {} -> {}", filename.to_string_lossy(), target.display());
        } else {
            fs::copy(dep, &dest)?;
            println!("  Copied {}", filename.to_string_lossy());

            // Patch rpath on the copied library so it can find other libs (use $ORIGIN since libs are in same dir)
            patch_rpath_with_value(&dest, "$ORIGIN")?;
        }
    }

    // Patch rpath to just $ORIGIN/lib
    patch_rpath_with_value(&binary_dest, "$ORIGIN/lib")?;

    // Copy SSL certificate bundle
    let cert_dest = output.join("etc/ssl/certs/ca-certificates.crt");
    fs::create_dir_all(cert_dest.parent().unwrap())?;
    fs::copy("/etc/ssl/certs/ca-certificates.crt", &cert_dest)?;
    println!("Copied SSL certificate bundle");

    Ok(())
}

fn collect_file_and_symlink_chain(path: &Path, deps: &mut HashSet<PathBuf>) -> Result<()> {
    // If we've already visited this exact path, skip
    if !deps.insert(path.to_path_buf()) {
        return Ok(());
    }

    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to get metadata for {}", path.display()))?;

    // If path is a symlink, follow it recursively to collect the target too
    if metadata.is_symlink() {
        let target = fs::read_link(path)?;
        let resolved_target = if target.is_absolute() {
            target
        } else {
            path.parent().unwrap().join(&target)
        };
        collect_file_and_symlink_chain(&resolved_target, deps)?;
    }

    Ok(())
}

fn get_dependencies(binary: &Path) -> Result<HashSet<PathBuf>> {
    // TODO: consider using goblin crate to parse ELF NEEDED entries directly instead of ldd
    let output = Command::new("ldd")
        .arg(binary)
        .output()?;

    if !output.status.success() {
        bail!("ldd failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut deps = HashSet::new();

    for line in stdout.lines() {
        let line = line.trim();

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
                collect_file_and_symlink_chain(Path::new(path_str), &mut deps)?;
            }
        }
    }

    Ok(deps)
}

fn patch_rpath_with_value(binary: &Path, rpath: &str) -> Result<()> {
    let status = Command::new("patchelf")
        .arg("--set-rpath")
        .arg(rpath)
        .arg(binary)
        .status()?;

    if !status.success() {
        bail!("patchelf failed with status: {}", status);
    }

    println!("Patched rpath");
    Ok(())
}
