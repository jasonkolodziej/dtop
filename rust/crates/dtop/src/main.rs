//! dtop: a Docker and Docker Compose terminal monitor.

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(dtop_app::run(&arguments));
}
