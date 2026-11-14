fn main() {
    println!("cargo:rerun-if-env-changed=P4_HTTP_SCRATCH_KIB");
    if let Ok(value) = std::env::var("P4_HTTP_SCRATCH_KIB") {
        assert!(
            ["8", "16", "32"].contains(&value.as_str()),
            "invalid P4_HTTP_SCRATCH_KIB"
        );
        println!("cargo:rustc-env=P4_HTTP_SCRATCH_KIB={value}");
    }
    println!("cargo:rustc-check-cfg=cfg(sd_bounce_experiment)");
    println!("cargo:rerun-if-env-changed=P4_SD_BOUNCE_KIB");
    if let Ok(value) = std::env::var("P4_SD_BOUNCE_KIB") {
        assert!(
            ["8", "16", "32"].contains(&value.as_str()),
            "invalid P4_SD_BOUNCE_KIB"
        );
        println!("cargo:rustc-env=P4_SD_BOUNCE_KIB={value}");
        println!("cargo:rustc-cfg=sd_bounce_experiment");
    }
    for (name, allowed) in [
        ("P4_UPLOAD_BLOCK_KIB", ["32", "64"]),
        ("P4_UPLOAD_BLOCKS", ["2", "3"]),
    ] {
        println!("cargo:rerun-if-env-changed={name}");
        if let Ok(value) = std::env::var(name) {
            assert!(allowed.contains(&value.as_str()), "invalid {name}");
            println!("cargo:rustc-env={name}={value}");
        }
    }
    println!("cargo:rerun-if-changed=link.x");
    for filename in ["commands.tsv", "command-routes.tsv"] {
        println!("cargo:rerun-if-changed={filename}");
        let all = std::fs::read_to_string(filename).unwrap();
        let mut active = String::new();
        for line in all
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            let fields: Vec<_> = line.split('\t').collect();
            assert!(
                (2..=3).contains(&fields.len()),
                "invalid registry row: {line}"
            );
            let enabled = fields.get(2).is_none_or(|features| {
                features.split(',').all(|feature| {
                    assert!(
                        [
                            "diagnostics",
                            "wifi",
                            "software-jpeg",
                            "audio-lab",
                            "hp-worker",
                            "lp-timer"
                        ]
                        .contains(&feature),
                        "unknown command feature"
                    );
                    if feature == "hp-worker"
                        && (std::env::var_os("CARGO_FEATURE_MULTICORE_LAB").is_some()
                            || std::env::var_os("CARGO_FEATURE_L2_128").is_some())
                    {
                        return false;
                    }
                    if feature == "lp-timer" && std::env::var_os("CARGO_FEATURE_LP_LAB").is_some() {
                        return false;
                    }
                    let env = format!("CARGO_FEATURE_{}", feature.replace('-', "_").to_uppercase());
                    std::env::var_os(env).is_some()
                })
            });
            if enabled {
                active.push_str(fields[0]);
                active.push('\t');
                active.push_str(fields[1]);
                active.push('\n');
            }
        }
        let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join(filename);
        std::fs::write(output, active).unwrap();
    }

    if std::env::var("TARGET").as_deref() == Ok("riscv32imafc-unknown-none-elf") {
        let mut link = std::fs::read_to_string("link.x").unwrap();
        if std::env::var_os("CARGO_FEATURE_WIFI").is_some() {
            link = link
                .replace("LENGTH = 136K", "LENGTH = 116K")
                .replace("0x4ff22000", "0x4ff1d000")
                .replace("LENGTH = 16K", "LENGTH = 36K");
        }

        if std::env::var_os("CARGO_FEATURE_L2_128").is_some() {
            assert_eq!(
                link.matches("LENGTH = 448K").count(),
                1,
                "unexpected arena template"
            );
            assert_eq!(
                link.matches("0x4ffb0000").count(),
                1,
                "unexpected stack template"
            );
            link = link
                .replace("LENGTH = 448K", "LENGTH = 320K")
                .replace("0x4ffb0000", "0x4ff90000");
        }
        let path = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("link.x");
        std::fs::write(&path, link).unwrap();
        println!("cargo:rustc-link-arg=-T{}", path.display());
    }
}
