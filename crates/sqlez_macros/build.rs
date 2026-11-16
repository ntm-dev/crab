//! Decides whether the `sql!` macro can validate SQL syntax at compile time.
//!
//! Validation needs the macro dylib to link sqlite. On some macOS releases (27+) dyld
//! refuses to `dlopen` that dylib (`mis-aligned LINKEDIT string pool`), so rustc
//! fails with `E0463: can't find crate for sqlez_macros`. On those hosts we emit
//! `sqlez_skip_sql_check` and the macro only formats the SQL, like on Linux/FreeBSD.
//!
//! Set `SQLEZ_FORCE_SQL_CHECK=1` to keep validation on, or `SQLEZ_SKIP_SQL_CHECK=1`
//! to turn it off on any host.

use std::process::Command;

/// First macOS release whose dyld rejects the sqlite-linked macro dylib.
const FIRST_AFFECTED_MACOS_MAJOR: u32 = 27;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(sqlez_skip_sql_check)");
    println!("cargo::rerun-if-env-changed=SQLEZ_FORCE_SQL_CHECK");
    println!("cargo::rerun-if-env-changed=SQLEZ_SKIP_SQL_CHECK");
    println!("cargo::rerun-if-changed=build.rs");

    let skip = if std::env::var_os("SQLEZ_FORCE_SQL_CHECK").is_some() {
        false
    } else if std::env::var_os("SQLEZ_SKIP_SQL_CHECK").is_some() {
        true
    } else {
        // The build script is compiled for the host, which is also where the proc macro runs.
        cfg!(target_os = "macos")
            && macos_major_version().is_some_and(|major| major >= FIRST_AFFECTED_MACOS_MAJOR)
    };

    if skip {
        println!("cargo::rustc-cfg=sqlez_skip_sql_check");
        println!(
            "cargo::warning=sqlez_macros: skipping compile-time SQL syntax checks on this macOS version"
        );
    }
}

fn macos_major_version() -> Option<u32> {
    let output = Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .split('.')
        .next()?
        .parse()
        .ok()
}
