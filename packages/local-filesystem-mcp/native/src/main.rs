fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let Err(error) = narada_local_filesystem_mcp::filesystem::run(&args) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
