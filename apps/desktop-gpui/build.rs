#[path = "../../scripts/diagnostic-build.rs"]
mod diagnostic_build;

fn main() {
    diagnostic_build::emit();
    println!("cargo:rerun-if-env-changed=SAYPLAY_BUILD");
    println!("cargo:rustc-check-cfg=cfg(sayplay_build)");
    let sayplay = std::env::var("SAYPLAY_BUILD").is_ok_and(|value| value == "1");
    if sayplay {
        println!("cargo:rustc-cfg=sayplay_build");
    }
    cap_i18n_build::build("locales");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-changed=native/exception_guard.m");
        cc::Build::new()
            .file("native/exception_guard.m")
            .flag("-fobjc-exceptions")
            .flag("-fno-objc-arc")
            .compile("cap_exception_guard");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // Export dispatch constructs media futures on the UI thread before Tokio can poll them.
        println!("cargo:rustc-link-arg-bin=cap-gpui=/STACK:16777216");
    }

    let icon = "icons/icon.ico";
    println!("cargo:rerun-if-changed={icon}");
    let (product_name, file_description, original_filename) = if sayplay {
        (
            "SayPlay Recorder",
            "SayPlay Recorder",
            "SayPlay Recorder.exe",
        )
    } else {
        ("Cap", "Cap", "Cap.exe")
    };
    tauri_winres::WindowsResource::new()
        .set_icon_with_id(icon, "1")
        .set("ProductName", product_name)
        .set("FileDescription", file_description)
        .set("OriginalFilename", original_filename)
        .compile()
        .expect("failed to compile the Cap Windows icon resource");
}
