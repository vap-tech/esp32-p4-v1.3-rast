fn main() {
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
                        ["diagnostics", "wifi", "software-jpeg", "audio-lab"].contains(&feature),
                        "unknown command feature"
                    );
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
