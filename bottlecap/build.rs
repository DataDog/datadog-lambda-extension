use std::process::Command;

fn main() {
    // Check if the "fips" feature is enabled
    let fips_enabled = std::env::var("CARGO_FEATURE_FIPS").is_ok();

    if fips_enabled {
        println!("cargo:warning=FIPS feature is enabled, checking for ring dependency...");

        // First run cargo tree to get dependency on ring with detailed info
        let output = Command::new("cargo")
            .args(&[
                "tree",
                "-i",
                "ring",
                "--format={p} {f}",
                "--prefix-depth",
                "--features=fips",
                "--no-default-features",
            ])
            .output()
            .expect("Failed to execute cargo tree command");

        // Also get the complete dependency path to help debugging
        let path_output = Command::new("cargo")
            .args(&[
                "tree",
                "-i",
                "ring",
                "--features=fips",
                "--no-default-features",
            ])
            .output()
            .expect("Failed to execute detailed cargo tree command");

        let output_str = String::from_utf8_lossy(&output.stdout);

        // Check if ring is in the dependency tree
        if output_str.contains("ring v") {
            // Get the dependency paths to ring
            let ring_deps: Vec<&str> = output_str
                .lines()
                .filter(|line| line.contains("ring v"))
                .collect();

            // Get the detailed dependency path
            let path_str = String::from_utf8_lossy(&path_output.stdout);

            // Print detailed error message with dependency paths
            let error_msg = format!(
                "\n\nERROR: ring dependency detected with FIPS feature enabled!\n\
                FIPS compliance requires eliminating all ring dependencies.\n\
                \n\
                Ring dependency versions and features:\n{}\n\
                \n\
                Detailed dependency paths to ring:\n{}\n\
                \n\
                Ensure all dependencies use aws-lc-rs instead of ring.\n\
                Consider updating the following in your Cargo.toml:\n\
                1. Ensure all dependencies that use rustls have the 'aws-lc-rs' feature\n\
                2. Check transitive dependencies in reqwest, hyper-rustls, etc.\n\
                3. Update your dependencies to versions that support FIPS mode\n",
                ring_deps.join("\n"),
                path_str
            );

            panic!("{}", error_msg);
        } else {
            println!("cargo:warning=No ring dependency found. FIPS compliance check passed!");
        }
    } else {
        println!("cargo:warning=FIPS feature is not enabled, skipping ring dependency check.");
    }
}
