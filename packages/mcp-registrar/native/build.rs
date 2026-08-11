fn main() {
    println!("cargo:rerun-if-changed=tool-catalog.json.gz");
}
