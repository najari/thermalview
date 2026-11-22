use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const SOURCES: &[&str] = &[
    "ctrl.c",
    "ctrl-gen.c",
    "device.c",
    "diag.c",
    "frame.c",
    "init.c",
    "misc.c",
    "stream.c",
];

/// Find libusb.h in the include directory exported by libusb1-sys (layout differs between versions).
fn find_libusb_header(dir: &Path) -> Option<PathBuf> {
    [
        dir.join("libusb.h"),
        dir.join("libusb-1.0").join("libusb.h"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor = manifest.join("vendor").join("libuvc");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    // libuvc.h does `#include <libusb-1.0/libusb.h>`: stage libusb1-sys's header under that name.
    let usb_include = env::var("DEP_USB_1.0_INCLUDE")
        .expect("libusb1-sys did not export DEP_USB_1.0_INCLUDE (is the `vendored` feature on?)");
    let header = find_libusb_header(Path::new(&usb_include))
        .unwrap_or_else(|| panic!("libusb.h not found under {usb_include}"));
    let staged = out.join("include").join("libusb-1.0");
    fs::create_dir_all(&staged).unwrap();
    fs::copy(&header, staged.join("libusb.h")).unwrap();

    let mut build = cc::Build::new();
    build
        .include(vendor.join("include"))
        .include(out.join("include"))
        .warnings(false);

    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // pthread shim; also silence the CRT "use strdup_s" deprecation noise.
        build
            .include(manifest.join("compat"))
            .define("_CRT_SECURE_NO_WARNINGS", None)
            .define("_CRT_NONSTDC_NO_DEPRECATE", None);
    }

    for src in SOURCES {
        build.file(vendor.join("src").join(src));
    }
    build.compile("uvc");

    println!("cargo:rerun-if-changed=vendor");
    println!("cargo:rerun-if-changed=compat");
}
