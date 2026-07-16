// The installed LLVM 22 reports `xml2s.lib` in `llvm-config --system-libs` (it
// was built with libxml2), but the redistributable doesn't ship that static lib.
// We don't use LLVM's XML paths, and the linker dead-strips unreferenced code
// (`/OPT:REF`), so an empty stub `xml2s.lib` satisfies the linker. Put its
// directory on the link search path.
fn main() {
    let stub = format!("{}/.llvm-stublib", env!("CARGO_MANIFEST_DIR"));
    if std::path::Path::new(&format!("{stub}/xml2s.lib")).exists() {
        println!("cargo:rustc-link-search=native={stub}");
    }
}
