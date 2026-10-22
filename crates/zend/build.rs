// Build the pinned sqlite-vec source into the kernel; no runtime extension downloads or loading.
fn main() {
    let include = std::env::var("DEP_SQLITE3_INCLUDE")
        .expect("bundled libsqlite3-sys must expose its SQLite headers");
    cc::Build::new()
        .file("vendor/sqlite-vec/sqlite-vec.c")
        .include(include)
        .include("vendor/sqlite-vec")
        .define("SQLITE_CORE", None)
        .define("SQLITE_VEC_STATIC", None)
        .define("SQLITE_VEC_OMIT_FS", None)
        .warnings(false)
        .compile("zen_sqlite_vec");
    println!("cargo:rerun-if-changed=vendor/sqlite-vec/sqlite-vec.c");
    println!("cargo:rerun-if-changed=vendor/sqlite-vec/sqlite-vec.h");
    #[cfg(unix)]
    println!("cargo:rustc-link-lib=m");
}
