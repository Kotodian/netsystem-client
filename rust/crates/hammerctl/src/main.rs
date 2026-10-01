use std::env;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

const DEFAULT_SOCKET: &str = "/run/hammer/cli.sock";

fn main() {
    if let Err(error) = run() {
        eprintln!("hammerctl: {error}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let mut arguments = env::args_os().skip(1);
    let mut socket = PathBuf::from(DEFAULT_SOCKET);
    let mut command = Vec::new();
    while let Some(argument) = arguments.next() {
        if command.is_empty() && (argument == "-s" || argument == "--socket") {
            socket = PathBuf::from(arguments.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "missing CLI socket path")
            })?);
        } else if command.is_empty() && (argument == "-h" || argument == "--help") {
            println!("Usage: hammerctl [-s SOCKET] [COMMAND ...]");
            return Ok(());
        } else {
            command.push(argument);
            command.extend(arguments);
            break;
        }
    }

    if command.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing command; use hammerctl [-s SOCKET] COMMAND ...",
        ));
    }
    let command = command
        .into_iter()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "command is not UTF-8"))?
        .join(" ");
    let stream = UnixStream::connect(&socket).map_err(|source| {
        io::Error::new(source.kind(), format!("connect {}: {source}", socket.display()))
    })?;
    execute(&mut BufReader::new(stream), &command)
}

fn execute(connection: &mut BufReader<UnixStream>, command: &str) -> io::Result<()> {
    connection.get_mut().write_all(command.as_bytes())?;
    connection.get_mut().write_all(b"\n")?;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    loop {
        let available = connection.fill_buf()?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "CLI connection closed before command completion",
            ));
        }
        let completed = available.iter().position(|byte| *byte == 0);
        let length = completed.unwrap_or(available.len());
        output.write_all(&available[..length])?;
        connection.consume(length + usize::from(completed.is_some()));
        if completed.is_some() {
            break;
        }
    }
    output.flush()
}
