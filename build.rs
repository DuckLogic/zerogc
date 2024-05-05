pub fn main() {
    if rustversion::cfg!(nightly) {
        println!("cargo:rustc-cfg=zerogc_next_nightly")
    }
}
