fn main() {
    println!("cargo:rerun-if-changed=link.x");
    if std::env::var("TARGET").as_deref() == Ok("riscv32imafc-unknown-none-elf") {
        println!("cargo:rustc-link-arg=-Tlink.x");
    }
}
