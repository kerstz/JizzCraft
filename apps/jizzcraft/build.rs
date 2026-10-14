// Embeds the icon, manifest and version information into jizzcraft.exe.
// Only runs when building on Windows; elsewhere there is nothing to do.
fn main() {
    println!("cargo:rerun-if-changed=windows/jizzcraft.rc");
    println!("cargo:rerun-if-changed=windows/jizzcraft.manifest");
    println!("cargo:rerun-if-changed=../../assets/icons/jizzcraft.ico");
    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            embed_resource::compile("windows/jizzcraft.rc", embed_resource::NONE)
                .manifest_optional()
                .expect("embed resources");
        }
    }
}
