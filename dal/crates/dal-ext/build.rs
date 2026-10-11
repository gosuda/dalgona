//! Build script: generate the embedded dal manual module.
#[path = "src/docsgen.rs"]
pub mod docsgen;

fn main() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    println!(
        "cargo:rerun-if-changed={}",
        root.join("docs/pages").display()
    );
    println!("cargo:rerun-if-changed={}", root.join("docs").display());
    println!(
        "cargo:rerun-if-changed={}",
        root.join("examples/plugins").display()
    );
    #[expect(
        clippy::disallowed_methods,
        clippy::expect_used,
        reason = "Cargo passes OUT_DIR to build scripts only through the environment; that state cannot happen under Cargo"
    )]
    let out = std::env::var_os("OUT_DIR")
        .map(|dir| std::path::PathBuf::from(dir).join("dal_docs_pages.rs"))
        .expect("OUT_DIR is set by cargo");
    let code = docsgen::cli(
        [
            "docs-gen",
            "--scheme",
            "dal",
            "--dir",
            &root.join("docs").to_string_lossy(),
            "--examples",
            &root.join("examples/plugins").to_string_lossy(),
            "--scan",
            &root.join("dal").to_string_lossy(),
            "--scan",
            &root.join("docs").to_string_lossy(),
            "--scan",
            &root.join("examples/plugins").to_string_lossy(),
            "--out",
            &out.to_string_lossy(),
        ]
        .into_iter()
        .map(str::to_owned),
    );
    assert_eq!(code, 0, "docs-gen failed");
}
