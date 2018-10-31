use std::env;

fn main() {
    let target = env::var("TARGET").unwrap();

    if target.starts_with("thumbv7m") | target.starts_with("thumbv7em") {
        println!("cargo:rustc-cfg=armv7m")
    }
}
