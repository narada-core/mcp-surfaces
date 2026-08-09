#[allow(dead_code)]
mod filesystem;
mod protocol;
mod rhai_filesystem;

use std::env;

fn main() {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some("rhai-filesystem") {
        args.remove(0);
    }
    if let Err(error) = rhai_filesystem::run(&args) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
