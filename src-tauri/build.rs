fn main() {
    println!("cargo:rerun-if-env-changed=INVOICE_UPDATE_MANIFEST_URL");
    println!("cargo:rerun-if-env-changed=INVOICE_ENABLE_CONCUR_SEND");
    tauri_build::build()
}
