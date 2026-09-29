fn main() {
    velopack::VelopackApp::build().run();
    if let Err(error) = desktop::run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
