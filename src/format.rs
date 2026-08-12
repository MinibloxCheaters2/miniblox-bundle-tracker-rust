use oxc_allocator::Allocator;
use oxc_formatter::{format, JsFormatOptions};
use oxc_span::SourceType;
use std::thread;

const STACK_SIZE: usize = 256 * 1024 * 1024;

/// Format a JS bundle with oxc. Runs on a thread with a large stack because
/// formatting the multi-megabyte bundle overflows the default stack.
pub fn format_source(source: &str) -> Result<String, String> {
    let src = source.to_string();
    let handle = thread::Builder::new()
        .stack_size(STACK_SIZE)
        .name("formatter".into())
        .spawn(move || {
            let allocator = Allocator::default();
            match format(&allocator, &src, SourceType::mjs(), JsFormatOptions::default()) {
                Ok(formatted) => {
                    let printed = formatted.print().map_err(|e| format!("print error: {e:?}"))?;
                    Ok(printed.into_code())
                }
                Err(e) => Err(format!("format error: {e:?}")),
            }
        })
        .map_err(|e| e.to_string())?;
    handle.join().map_err(|e| format!("formatter thread panicked: {e:?}"))?
}
