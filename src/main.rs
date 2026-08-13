mod fetch;
mod fingerprint;
mod format;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::ExitCode;

const EXIT_CHANGED: u8 = 10;

struct Args {
    out: String,
    previous: Option<String>,
    local: Option<String>,
    print_fingerprint: bool,
    print_changed: bool,
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
                    if args.print_changed {
                        print_changed(&asset, &prev_fp, &current);
                    }
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
    } else if args.print_changed {
        println!("export changes: no previous bundle to compare against");
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

/// Diff the export blocks of the previous and current fingerprints. Blocks are
/// matched by their semantic content (rename-stable), so a pure minifier
/// rename of an export never appears. Only the obfuscated export symbols whose
/// content actually changed are printed, paired by name as "modified".
fn print_changed(asset: &str, prev_fp: &str, cur_fp: &str) {
    let prev = parse_blocks(prev_fp);
    let cur = parse_blocks(cur_fp);

    let mut prev_rem: HashMap<&str, usize> = HashMap::new();
    for b in &prev {
        *prev_rem.entry(b.1.as_str()).or_insert(0) += 1;
    }
    let mut cur_rem: HashMap<&str, usize> = HashMap::new();
    for b in &cur {
        *cur_rem.entry(b.1.as_str()).or_insert(0) += 1;
    }
    for key in prev_rem.keys().copied().collect::<Vec<_>>() {
        if let Some(&cur_n) = cur_rem.get(key) {
            let prev_n = prev_rem.remove(key).unwrap();
            if cur_n > prev_n {
                cur_rem.insert(key, cur_n - prev_n);
            } else {
                cur_rem.remove(key);
            }
        }
    }

    let removed_content: Vec<(&str, &str)> = prev
        .iter()
        .filter(|b| prev_rem.contains_key(b.1.as_str()))
        .map(|b| (b.0.as_str(), b.1.as_str()))
        .collect();
    let added_content: Vec<(&str, &str)> = cur
        .iter()
        .filter(|b| cur_rem.contains_key(b.1.as_str()))
        .map(|b| (b.0.as_str(), b.1.as_str()))
        .collect();

    let mut modified: Vec<(&str, &str)> = Vec::new();
    let mut paired: HashSet<&str> = HashSet::new();
    for &(name, _) in &removed_content {
        if let Some(&(_, new_entries)) = added_content.iter().find(|&(n, _)| *n == name) {
            modified.push((name, new_entries));
            paired.insert(name);
        }
    }
    let mut added_out: Vec<&str> =
        added_content.iter().filter(|&&(n, _)| !paired.contains(n)).map(|&(n, _)| n).collect();
    let mut removed_out: Vec<&str> =
        removed_content.iter().filter(|&&(n, _)| !paired.contains(n)).map(|&(n, _)| n).collect();
    modified.sort();
    added_out.sort();
    removed_out.sort();

    println!("export changes vs previous ({asset}):");
    if modified.is_empty() && added_out.is_empty() && removed_out.is_empty() {
        println!("  none");
        return;
    }
    if !modified.is_empty() {
        println!("  modified ({}):", modified.len());
        for (name, entries) in modified {
            println!("    {name}");
            for line in entries.lines() {
                println!("      {line}");
            }
        }
    }
    if !added_out.is_empty() {
        println!("  added ({}):", added_out.len());
        for name in added_out {
            println!("    {name}");
        }
    }
    if !removed_out.is_empty() {
        println!("  removed ({}):", removed_out.len());
        for name in removed_out {
            println!("    {name}");
        }
    }
}

/// Split a fingerprint string into `(export name, semantic entries)` blocks.
/// The fingerprint format is one export per block: `name`, entries, blank line.
fn parse_blocks(fp: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for block in fp.split("\n\n") {
        if block.is_empty() {
            continue;
        }
        let mut it = block.splitn(2, '\n');
        let name = it.next().unwrap_or_default().to_string();
        let entries = it.next().unwrap_or_default().to_string();
        out.push((name, entries));
    }
    out
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
        print_changed: false,
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
            "--print-changed" => args.print_changed = true,
            other => {
                eprintln!("error: unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    args
}
