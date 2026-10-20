//! `cargo run -p ts_r_transform --example transpile -- file.ts ['{"module":"commonjs"}'] [--map] [--tsc]`
//! prints the transpiled JavaScript (and the source map with `--map`; tsc's layout with `--tsc`).

use ts_r_codegen::{CodegenOptions, Layout, SourceMapOptions};
use ts_r_transform::{TransformOptions, transpile};

ts_r_testing::cap_memory!();

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: transpile <file> [compilerOptions-json] [--map] [--tsc]");
        std::process::exit(2);
    };
    let json = args
        .iter()
        .skip(1)
        .find(|a| a.starts_with('{'))
        .map_or("{}", String::as_str);
    let map = args.iter().any(|a| a == "--map");
    let tsc = args.iter().any(|a| a == "--tsc");
    let src = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    let opts = TransformOptions::from_json(json, path);
    let codegen = CodegenOptions {
        source_map: map.then(|| SourceMapOptions {
            file: None,
            source: path.clone(),
            source_root: None,
            include_content: false,
        }),
        layout: if tsc { Layout::Tsc } else { Layout::Readable },
        ..CodegenOptions::default()
    };
    let out = transpile(path, &src, &opts, &codegen);
    for d in &out.diagnostics {
        eprintln!("{}: TS{}: {}", path, d.code, d.message);
    }
    print!("{}", out.code);
    if let Some(m) = out.map {
        println!("{}", m.to_json());
    }
}
