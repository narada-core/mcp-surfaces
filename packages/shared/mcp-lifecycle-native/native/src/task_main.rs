use narada_mcp_lifecycle::{LifecycleServer, Options, Surface};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match Options::parse(Surface::Task, &args) {
        Err(error) if error == "__help__" => {
            println!("{}", Options::usage(Surface::Task));
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
        Ok(options) => match LifecycleServer::new(options).and_then(|mut server| server.run_stdio()) {
            Ok(()) => {}
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        },
    }
}
