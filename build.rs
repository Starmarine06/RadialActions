// Embeds the icon and version details in the exe.
fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico")
            .set("ProductName", "RadialActions")
            .set("FileDescription", "RadialActions")
            .set("LegalCopyright", "MIT licence");
        res.compile().expect("resource compiler");
    }
}
