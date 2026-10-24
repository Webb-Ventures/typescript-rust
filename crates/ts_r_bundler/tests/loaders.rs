//! `data:` URL modules, file-loader asset paths and URLs, and `public_path` for chunk
//! imports, as the esbuild bundler oracle checks them (`bundler_loader_test.go`,
//! `bundler_splitting_test.go`).

ts_r_testing::cap_memory!();

use std::sync::Arc;

use ts_r_bundler::{BuildContext, BuildOptions, BuildResult, EntryPoint, Format, Loader, Platform};
use ts_r_config::MemoryFs;

fn project(files: &[(&str, &str)]) -> MemoryFs {
    let mut fs = MemoryFs::new();
    for (p, c) in files {
        fs.add_file(&format!("/proj/{p}"), c);
    }
    fs
}

fn opts(entries: &[&str]) -> BuildOptions {
    BuildOptions {
        entry_points: entries
            .iter()
            .map(|e| EntryPoint {
                path: (*e).into(),
                name: None,
            })
            .collect(),
        working_dir: "/proj".into(),
        bundle: true,
        outdir: Some("out".into()),
        write: false,
        threads: 2,
        platform: Platform::Node,
        format: Some(Format::Esm),
        ..BuildOptions::default()
    }
}

fn build(fs: &MemoryFs, o: BuildOptions) -> BuildResult {
    BuildContext::new(o, Arc::new(fs.clone()))
        .expect("options")
        .build()
}

fn errors(r: &BuildResult) -> Vec<String> {
    r.messages
        .iter()
        .filter(|m| m.is_error())
        .map(|m| m.text.clone())
        .collect()
}

fn output<'a>(r: &'a BuildResult, suffix: &str) -> &'a str {
    assert!(errors(r).is_empty(), "{:?}", errors(r));
    r.outputs
        .iter()
        .find(|o| o.path.ends_with(suffix))
        .unwrap_or_else(|| {
            let paths: Vec<&str> = r.outputs.iter().map(|o| o.path.as_str()).collect();
            panic!("no output {suffix} in {paths:?}")
        })
        .text()
}

#[test]
fn data_url_modules() {
    let fs = project(&[
        (
            "entry.js",
            "import \"data:text/javascript,console.log('%31%32%33', 1+2)\";\n\
             import \"data:text/javascript;base64,Y29uc29sZS5sb2coMjM0KQ==\";\n\
             import j from 'data:application/json;charset=UTF-8,{\"works\":true}';\n\
             import u from 'data:some/thing,x';\n\
             console.log(j, u);\n",
        ),
        ("other.js", "shouldNotBeImported = true"),
    ]);
    let r = build(&fs, opts(&["entry.js"]));
    let js = output(&r, "entry.js");
    assert!(js.contains("console.log('123', 1 + 2)"), "{js}");
    assert!(js.contains("console.log(234)"), "{js}");
    assert!(js.contains("\"works\": true"), "{js}");
    // Unknown MIME types stay imports.
    assert!(js.contains("from \"data:some/thing,x\""), "{js}");
    // A data: URL module has no directory to import from.
    let fs = project(&[
        (
            "entry.js",
            "import \"data:text/javascript,import './other.js'\";",
        ),
        ("other.js", "shouldNotBeImported = true"),
    ]);
    let r = build(&fs, opts(&["entry.js"]));
    assert_eq!(errors(&r), ["Could not resolve \"./other.js\""]);
}

#[test]
fn file_loader_paths_are_relative_to_each_chunk() {
    let fs = project(&[
        ("src/entries/entry.js", "import '../shared/common.js'"),
        (
            "src/entries/other/entry.js",
            "import '../../shared/common.js'",
        ),
        (
            "src/shared/common.js",
            "import x from './common.png'; console.log(x)",
        ),
        ("src/shared/common.png", "x"),
    ]);
    let mut o = opts(&["src/entries/entry.js", "src/entries/other/entry.js"]);
    o.loaders.insert(".png".into(), Loader::File);
    let r = build(&fs, o.clone());
    let a = output(&r, "out/entry.js");
    let b = output(&r, "out/other/entry.js");
    let url = |s: &str| {
        s.split('"')
            .find(|p| p.ends_with(".png"))
            .unwrap_or("")
            .to_owned()
    };
    assert!(url(a).starts_with("./common-"), "{a}");
    assert!(url(b).starts_with("../common-"), "{b}");
    assert!(r.outputs.iter().any(|f| f.path.ends_with(&url(a)[1..])));

    // `[dir]` is relative to the entries' common directory, `..` spelled `_.._`.
    let fs = project(&[
        (
            "src/entries/entry.js",
            "import x from '../images/image.png'; console.log(x)",
        ),
        ("src/images/image.png", "x"),
    ]);
    let mut o = opts(&["src/entries/entry.js"]);
    o.loaders.insert(".png".into(), Loader::File);
    o.asset_names = "[dir]/[name]-[hash]".into();
    let r = build(&fs, o.clone());
    let js = output(&r, "out/entry.js");
    assert!(js.contains("\"./_.._/images/image-"), "{js}");
    assert!(
        r.outputs
            .iter()
            .any(|f| f.path.contains("/out/_.._/images/image-"))
    );
    o.public_path = "https://example.com".into();
    let js = output(&build(&fs, o), "out/entry.js").to_owned();
    assert!(
        js.contains("\"https://example.com/_.._/images/image-"),
        "{js}"
    );
}

#[test]
fn query_suffixes_are_separate_modules() {
    let fs = project(&[
        (
            "entry.js",
            "import foo from './file.txt?foo'; import bar from './file.txt?bar'; console.log(foo, bar)",
        ),
        ("file.txt", "This is some text"),
    ]);
    let mut o = opts(&["entry.js"]);
    o.loaders.insert(".txt".into(), Loader::File);
    let r = build(&fs, o);
    let js = output(&r, "entry.js");
    assert!(js.contains(".txt?foo\""), "{js}");
    assert!(js.contains(".txt?bar\""), "{js}");
    // One asset file for both.
    assert_eq!(
        r.outputs
            .iter()
            .filter(|f| f.path.ends_with(".txt"))
            .count(),
        1
    );
}

#[test]
fn public_path_prefixes_chunk_imports() {
    let fs = project(&[("a.js", "import('./b')"), ("b.js", "console.log('b')")]);
    let mut o = opts(&["a.js"]);
    o.splitting = true;
    o.public_path = "/www/".into();
    let r = build(&fs, o);
    let js = output(&r, "out/a.js");
    assert!(js.contains("import(\"/www/b.js\")"), "{js}");
}
