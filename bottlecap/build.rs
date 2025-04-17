use std::process::Command;

/// Checks if a specific dependency is present in the dependency tree when FIPS is enabled
fn check_forbidden_dependency(dependency_name: &str) -> Result<(), String> {
    println!(
        "cargo:warning=Checking for {} dependency...",
        dependency_name
    );

    // First run cargo tree to get dependency with detailed info
    let output = Command::new("cargo")
        .args([
            "tree",
            "-i",
            dependency_name,
            "--format={p} {f}",
            "--prefix-depth",
            "--features=fips",
            "--no-default-features",
        ])
        .output()
        .map_err(|e| {
            format!(
                "Failed to execute cargo tree command for {}: {}",
                dependency_name, e
            )
        })?;

    // Also get the complete dependency path to help debugging
    let path_output = Command::new("cargo")
        .args([
            "tree",
            "-i",
            dependency_name,
            "--features=fips",
            "--no-default-features",
        ])
        .output()
        .map_err(|e| {
            format!(
                "Failed to execute detailed cargo tree command for {}: {}",
                dependency_name, e
            )
        })?;

    let output_str = String::from_utf8_lossy(&output.stdout);
    let dependency_pattern = format!("{} v", dependency_name);

    // Check if the dependency is in the dependency tree
    if output_str.contains(&dependency_pattern) {
        // Get the dependency paths
        let deps: Vec<&str> = output_str
            .lines()
            .filter(|line| line.contains(&dependency_pattern))
            .collect();

        // Get the detailed dependency path
        let path_str = String::from_utf8_lossy(&path_output.stdout);

        // Create detailed error message with dependency paths
        let error_msg = format!(
            "\n\nERROR: {} dependency detected with FIPS feature enabled!\n\
            FIPS compliance requires eliminating this dependency.\n\
            \n\
            {} dependency versions and features:\n{}\n\
            \n\
            Detailed dependency paths to {}:\n{}\n\
            \n\
            Ensure all dependencies use aws-lc-rs instead of non-FIPS compliant cryptographic libraries.\n\
            Consider updating the following in your Cargo.toml:\n\
            1. Ensure all dependencies that use rustls have the 'aws-lc-rs' feature\n\
            2. Check transitive dependencies in reqwest, hyper-rustls, etc.\n\
            3. Update your dependencies to versions that support FIPS mode\n",
            dependency_name,
            dependency_name,
            deps.join("\n"),
            dependency_name,
            path_str
        );

        Err(error_msg)
    } else {
        println!("cargo:warning=No {} dependency found. FIPS compliance check passed for this dependency!", dependency_name);
        Ok(())
    }
}

fn main() {
    // Check if the "fips" feature is enabled
    let fips_enabled = std::env::var("CARGO_FEATURE_FIPS").is_ok();

    if fips_enabled {
        println!("cargo:warning=FIPS feature is enabled, checking for forbidden dependencies...");

        // List of dependencies that are not FIPS compliant
        let forbidden_dependencies = vec!["ring", "openssl", "boringssl"];

        // Check each forbidden dependency
        for dependency in &forbidden_dependencies {
            if let Err(error_msg) = check_forbidden_dependency(dependency) {
                panic!("{}", error_msg);
            }
        }

        println!("cargo:warning=All dependency checks passed. No forbidden dependencies found!");
    } else {
        println!("cargo:warning=FIPS feature is not enabled, skipping dependency checks.");
    }
}
