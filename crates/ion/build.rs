//! Embeds the app icon in the Windows executable.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("resources/ion.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}
