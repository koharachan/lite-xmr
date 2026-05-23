fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV")
        .map(|e| e == "gnu")
        .unwrap_or(false)
    {
        println!("cargo:rustc-link-lib=dylib=stdc++");
        println!("cargo:rustc-link-lib=static=stdc++");
    }
}
