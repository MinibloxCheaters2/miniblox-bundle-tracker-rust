mod fetch;
mod fingerprint;
mod format;

use std::fs;
use std::path::Path;
use std::process::ExitCode;

const EXIT_CHANGED: u8 = 10;

struct Args {
    out: String,
    previous: Option<String>,
    local: Option<String>,
    print_fingerprint: bool,
}

fn main() -> ExitCode {
    let args = parse_args();

    // 1. Obtain the raw bundle.
    let (asset, source) = match &args.local {
        Some(path) => {
            let text = match fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: cannot read {path}: {e}");
                    return ExitCode::FAILURE;
                }
            };
            (Path::new(path).file_name().unwrap_or_default().to_string_lossy().into_owned(), text)
        }
        None => match fetch::fetch_current() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        },
    };

    // 2. Format.
    let formatted = match format::format_source(&source) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // 3. Fingerprint.
    let current = match fingerprint::fingerprint(&formatted) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: fingerprint: {e}");
            return ExitCode::FAILURE;
        }
    };

    if args.print_fingerprint {
        println!("asset: {asset}");
        println!("raw bytes: {}, formatted bytes: {}", source.len(), formatted.len());
        println!("fingerprint: {:016x} ({} bytes)", fnv64(&current), current.len());
        println!("----");
        print!("{current}");
        return ExitCode::SUCCESS;
    }

    // 4. Compare against the previous bundle, if any.
    let previous_path = args
        .previous
        .clone()
        .or_else(|| {
            let p = Path::new(&args.out);
            if p.exists() { Some(args.out.clone()) } else { None }
        });

    if let Some(prev_path) = previous_path {
        match fs::read_to_string(&prev_path) {
            Ok(prev) => match fingerprint::fingerprint(&prev) {
                Ok(prev_fp) => {
                    if prev_fp == current {
                        println!(
                            "unchanged  {asset}: fp {:016x}, {} exports",
                            fnv64(&current),
                            export_count(&current)
                        );
                        return ExitCode::SUCCESS;
                    }
                }
                Err(e) => {
                    eprintln!("warning: cannot fingerprint previous bundle {prev_path}: {e}");
                }
            },
            Err(e) => {
                eprintln!("warning: cannot read previous bundle {prev_path}: {e}");
            }
        }
    }

    // 5. Write.
    let tmp = format!("{}.tmp", args.out);
    if let Err(e) = fs::write(&tmp, &formatted) {
        eprintln!("error: cannot write {tmp}: {e}");
        return ExitCode::FAILURE;
    }
    if let Err(e) = fs::rename(&tmp, &args.out) {
        eprintln!("error: cannot rename {tmp} -> {}: {e}", args.out);
        return ExitCode::FAILURE;
    }

    println!(
        "changed   {asset}: {} bytes -> {} bytes, fp {:016x}, {} exports",
        source.len(),
        formatted.len(),
        fnv64(&current),
        export_count(&current)
    );
    ExitCode::from(EXIT_CHANGED)
}

fn export_count(fp: &str) -> usize {
    fp.lines().filter(|l| l.is_empty()).count()
}

fn fnv64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn parse_args() -> Args {
    let mut args = Args {
        out: "bundle.js".into(),
        previous: None,
        local: None,
        print_fingerprint: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => {
                args.out = it.next().unwrap_or_else(|| {
                    eprintln!("error: --out needs a value");
                    std::process::exit(2);
                });
            }
            "--previous" => {
                args.previous = Some(it.next().unwrap_or_else(|| {
                    eprintln!("error: --previous needs a value");
                    std::process::exit(2);
                }));
            }
            "--local" => {
                args.local = Some(it.next().unwrap_or_else(|| {
                    eprintln!("error: --local needs a value");
                    std::process::exit(2);
                }));
            }
            "--print-fingerprint" => args.print_fingerprint = true,
            other => {
                eprintln!("error: unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    args
}
