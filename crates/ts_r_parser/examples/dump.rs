//! Prints the AST dump and diagnostics for a file or for `-e <source>`.
//!
//! ```text
//! cargo run -p ts_r_parser --example dump -- path/to/file.ts
//! cargo run -p ts_r_parser --example dump -- -e 'let x = <T>(a: T) => a' [--kind tsx]
//! cargo run -p ts_r_parser --example dump -- file.js --ts-in-js report   # tsc on .js
//! ```

// Real-world or generated input: abort instead of exhausting the machine's memory.
ts_r_testing::cap_memory!();

use ts_r_diagnostics::format_plain;
use ts_r_parser::ts_r_ast::SourceKind;
use ts_r_span::SourceText;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut kind = None;
    let mut name = String::from("input.ts");
    let mut text = None;
    let mut opts = ts_r_parser::ParseOptions::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--ts-in-js" => {
                i += 1;
                opts.ts_in_js = match args.get(i).map(String::as_str) {
                    Some("report") => ts_r_parser::TsInJs::Report,
                    Some("allow") => ts_r_parser::TsInJs::Allow,
                    _ => ts_r_parser::TsInJs::Reject,
                };
            }
            "-e" => {
                i += 1;
                text = args.get(i).cloned();
            }
            "--kind" => {
                i += 1;
                kind = args
                    .get(i)
                    .map(|k| SourceKind::from_path(&format!("x.{k}")));
            }
            path => {
                name = path.to_owned();
                text = std::fs::read_to_string(path).ok();
            }
        }
        i += 1;
    }
    let Some(text) = text else {
        eprintln!(
            "usage: dump <file> | dump -e <source> [--kind ts|tsx|js|d.ts] [--ts-in-js report|allow]"
        );
        std::process::exit(2);
    };
    let kind = kind.unwrap_or_else(|| SourceKind::from_path(&name));
    let r = ts_r_parser::parse_with(&text, kind, opts);
    print!("{}", r.ast.dump_with_source(&text));
    let src = SourceText::new(name, text);
    for d in &r.diagnostics {
        println!("{}", format_plain(d, &src));
    }
}
