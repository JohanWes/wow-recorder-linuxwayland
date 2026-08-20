// SPDX-License-Identifier: GPL-3.0-or-later

//! Compiles the one shared GResource bundle (CSS, category icons, product
//! mark, icon license notices) declared by `data/resources.gresource.xml`.
//! Shells out to the GLib tool instead of adding a build dependency.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let xml = manifest_dir.join("../data/resources.gresource.xml");
    let xml = xml
        .canonicalize()
        .unwrap_or_else(|error| panic!("read {}: {error}", xml.display()));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("out dir"));
    let output = out_dir.join("warcraft-recorder.gresource");

    println!("cargo:rerun-if-changed={}", xml.display());
    for entry in std::fs::read_dir(xml.with_file_name("assets/icons")).expect("list icon assets") {
        let entry = entry.expect("icon asset entry");
        println!("cargo:rerun-if-changed={}", entry.path().display());
    }
    // The spell database: the JSON plus every bundled spell icon.
    let spells = xml.with_file_name("spells");
    if let Ok(entries) = std::fs::read_dir(&spells) {
        for entry in entries.flatten() {
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("src/ui/style.css").display()
    );

    let status = Command::new("glib-compile-resources")
        .arg("--sourcedir")
        .arg(xml.parent().expect("resource xml directory"))
        .arg("--sourcedir")
        .arg(manifest_dir.join("src/ui"))
        .arg(format!("--target={}", output.display()))
        .arg(&xml)
        .status()
        .expect("run glib-compile-resources (provided by the GLib SDK)");
    assert!(status.success(), "glib-compile-resources failed");
}
