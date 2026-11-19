use std::io::{self, Write};

fn main() -> std::process::ExitCode {
    if std::env::args_os().len() != 1 {
        eprintln!("Usage: pebble-extract-key > mac-key.txt");
        return std::process::ExitCode::from(2);
    }
    match pebble_mac_key::extract_key() {
        Ok(key) => match io::stdout().lock().write_all(key.as_bytes()) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                std::process::ExitCode::SUCCESS
            }
            Err(_) => {
                eprintln!("Could not write the Mac key");
                std::process::ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
