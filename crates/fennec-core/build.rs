//! One name for the mapped path (docs/design/windows.md, decision 1): the
//! file is mapped where `std-fs` is on and the target is a 64-bit Unix or
//! Windows. Written out at each of its thirty-odd places, the condition
//! said what it was made of and not what it meant, and a target joining it
//! changed every one of them; `cfg(mapped)` says it, and joins here. No
//! crate is taken for it: the cfg is the one line Cargo reads below.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(mapped)");
    println!("cargo::rerun-if-changed=build.rs");
    let std_fs = std::env::var_os("CARGO_FEATURE_STD_FS").is_some();
    let family = std::env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    let systems = family.split(',').any(|f| f == "unix" || f == "windows");
    let wide = std::env::var("CARGO_CFG_TARGET_POINTER_WIDTH").as_deref() == Ok("64");
    if std_fs && systems && wide {
        println!("cargo::rustc-cfg=mapped");
    }
}
