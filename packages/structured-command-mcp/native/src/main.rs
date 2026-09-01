fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = if args.first().map(String::as_str) == Some("structured-command-background") {
        narada_structured_command_mcp::structured_command::run_background(&args[1..])
    } else {
        narada_structured_command_mcp::structured_command::run(&args)
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
