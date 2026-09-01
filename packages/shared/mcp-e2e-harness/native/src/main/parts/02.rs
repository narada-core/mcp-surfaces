#[cfg(not(windows))]
fn main() {
    eprintln!("narada-test-process-scope is currently implemented for Windows only");
    std::process::exit(70);
}

#[cfg(windows)]
fn main() {
    windows_scope::main();
}
