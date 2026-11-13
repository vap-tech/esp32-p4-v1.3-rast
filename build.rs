fn main() {
    println!("cargo:rerun-if-changed=link.x");
    if std::env::var("TARGET").as_deref() == Ok("riscv32imafc-unknown-none-elf") {
        let mut link = std::fs::read_to_string("link.x").unwrap();
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
