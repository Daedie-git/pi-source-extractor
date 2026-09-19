use pi_source_extractor::{Extractor, MAX_REQUEST_BYTES, Request, Response};
use std::io::{self, BufRead, Read, Write};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--stdio" => {}
            "--threads" => threads = args.next().ok_or("missing thread count")?.parse()?,
            "--help" | "-h" => {
                println!(
                    "pi-source-extractor [--stdio] [--threads 1..32]\nJSONL stdin: {{\"id\":1,\"files\":[{{\"path\":\"example.cpp\",\"source\":\"int f() {{ return 1; }}\"}}]}}\nOne ordered JSON response per request. Default: up to 4 threads; small batches stay serial."
                );
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    let mut extractor = Extractor::new(threads)?;
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = input
            .by_ref()
            .take((MAX_REQUEST_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        if line.len() > MAX_REQUEST_BYTES {
            serde_json::to_writer(
                &mut output,
                &Response::error(None, "request exceeds 16 MiB"),
            )?;
            output.write_all(b"\n")?;
            output.flush()?;
            return Err("oversized request; worker closed".into());
        }
        let response = match serde_json::from_slice::<Request>(&line) {
            Ok(request) => extractor.process(request),
            Err(error) => Response::error(None, format!("invalid request: {error}")),
        };
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
