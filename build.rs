fn main() {
    println!("cargo:rerun-if-changed=res/widepeek.rc");
    println!("cargo:rerun-if-changed=res/widepeek.manifest");
    println!("cargo:rerun-if-changed=res/widepeek.ico");
    embed_resource::compile("res/widepeek.rc", embed_resource::NONE)
        .manifest_required()
        .expect("failed to compile Windows resources");
}
