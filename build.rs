// Embeds Info.plist in the binary (__TEXT,__info_plist), so macOS knows the app's name and identifier without an
// app bundle.

use std::path::Path;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = Path::new(&manifest).join("Info.plist");
        println!("cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}", plist.display());
        println!("cargo:rerun-if-changed=Info.plist");
    }
}
