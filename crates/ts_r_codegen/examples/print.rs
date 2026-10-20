//! Parses a file and prints it back: `cargo run -p ts_r_codegen --example print -- <file> [--compact]`.

ts_r_testing::cap_memory!();

fn main() {
    let mut path = None;
    let mut compact = false;
    for a in std::env::args().skip(1) {
        if a == "--compact" {
            compact = true;
        } else {
            path = Some(a);
        }
    }
    let Some(path) = path else {
        eprintln!("usage: print <file> [--compact]");
        std::process::exit(2);
    };
    let src = std::fs::read_to_string(&path).unwrap_or_default();
    let parsed = ts_r_parser::parse_path(&path, &src);
    for d in &parsed.diagnostics {
        eprintln!("{}: {}", d.span.start, d.message);
    }
    let opts = ts_r_codegen::CodegenOptions {
        layout: if compact {
            ts_r_codegen::Layout::Compact
        } else {
            ts_r_codegen::Layout::Readable
        },
        ..Default::default()
    };
    print!("{}", ts_r_codegen::print(&parsed.ast, &src, &opts).code);
}
