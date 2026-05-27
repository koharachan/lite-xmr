fn main() {
    println!("cargo:rerun-if-changed=src/bridge/native_bridge.cpp");
    println!("cargo:rerun-if-changed=src/3rdparty/rapidjson");
    println!("cargo:rerun-if-changed=src/crypto/randomx/randomx.h");
    println!("cargo:rerun-if-changed=src/crypto/randomx/configuration.h");

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .file("src/bridge/native_bridge.cpp")
        .include("src")
        .flag_if_supported("/std:c++17")
        .flag_if_supported("-std=c++17")
        .compile("lite_xmr_native_bridge");

    if std::env::var("CARGO_CFG_TARGET_ENV")
        .map(|e| e == "gnu")
        .unwrap_or(false)
    {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }
}
