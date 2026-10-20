//! Prints the binder dump (and with `--flow`, the flow graph) of a file.
//! `cargo run -p ts_r_binder --example binddump -- [--flow] [--unreachable] file.ts`

use ts_r_ast::SourceKind;
use ts_r_binder::{BindOptions, bind};

fn main() {
    let mut flow = false;
    let mut opts = BindOptions::default();
    let mut path = None;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--flow" => flow = true,
            "--unreachable" => {
                opts.allow_unreachable_code = Some(false);
                opts.allow_unused_labels = Some(false);
            }
            _ => path = Some(a),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: binddump [--flow] [--unreachable] <file>");
        return;
    };
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{path}: {e}");
            return;
        }
    };
    let parsed = ts_r_parser::parse(&src, SourceKind::from_path(&path));
    for d in &parsed.diagnostics {
        println!("parse: TS{} {}", d.code, d.message);
    }
    let bound = bind(&parsed.ast, &src, &opts);
    print!("{}", bound.dump(&parsed.ast, &src));
    if flow {
        print!("{}", bound.dump_flow(&parsed.ast, &src));
    }
}
