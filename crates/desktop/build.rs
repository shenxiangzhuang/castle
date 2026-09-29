fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        println!("cargo:rerun-if-changed=app.rc");
        println!("cargo:rerun-if-changed=assets/app-icon.ico");
        embed_resource::compile("app.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
