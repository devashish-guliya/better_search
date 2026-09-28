use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("release");
    for filename in ["bs-service.exe", "bs-window.exe", "bs.exe"] {
        let path = root.join(filename);
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = std::fs::read(&path).unwrap_or_else(|err| {
            panic!(
                "{} missing ({err}); build the release workspace first",
                path.display()
            )
        });
        assert!(
            bytes.len() > 1024 && bytes.starts_with(b"MZ"),
            "{} is not a release Windows executable",
            path.display()
        );
    }
}
