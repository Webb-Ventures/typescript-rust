//! The parsed-file cache shared by an expander's expansions: overlapping import closures are
//! parsed once, a changed file is re-parsed, and unchanged stamped files are not even read.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ts_r_config::fs::DirEntry;
use ts_r_config::{FileStamp, FileSystem, MemoryFs};
use ts_r_guard::{ExpandOptions, Expander, GuardOptions, Layout};

ts_r_testing::cap_memory!();

fn expander(fs: Arc<dyn FileSystem>) -> Expander {
    Expander::new(
        fs,
        ExpandOptions {
            guard: GuardOptions::default(),
            layout: Layout::Inline,
        },
    )
}

/// Module `i` of a chain of `n`: validates its own type and imports a value of module `i + 1`,
/// so module 0's import closure is the whole chain.
fn chain_module(i: usize, n: usize) -> String {
    let next = if i + 1 < n {
        format!("import {{ v as next }} from \"./m{}\";\n", i + 1)
    } else {
        "const next = 0;\n".to_owned()
    };
    format!(
        "import {{ is }} from \"@ts-r.dev/guard\";\n{next}export interface L{i} {{ n: number }}\n\
         export const v = next + 1;\nexport const f = (x: unknown) => is<L{i}>(x);\n"
    )
}

#[test]
fn a_chain_of_modules_is_parsed_once_not_once_per_expansion() {
    // Before the cache each expansion parsed its whole closure: n(n+1)/2 = 20,100 parses.
    const N: usize = 200;
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/tsconfig.json", "{}");
    let srcs: Vec<String> = (0..N).map(|i| chain_module(i, N)).collect();
    for (i, s) in srcs.iter().enumerate() {
        fs.add_file(&format!("/p/m{i}.ts"), s);
    }
    let ex = expander(Arc::new(fs));
    for (i, s) in srcs.iter().enumerate() {
        let x = ex.expand(&format!("/p/m{i}.ts"), s).expect("expanded");
        assert!(x.errors.is_empty(), "m{i}: {:?}", x.errors);
        assert!(x.code.contains(&format!("$is_L{i}")), "m{i}: {}", x.code);
        // The closure is still complete (deps are every other file of it).
        assert_eq!(x.deps.len(), N - 1 - i, "m{i}");
    }
    assert_eq!(ex.parsed_files(), N);
}

/// An in-memory project that can change between expansions, counting reads; with `stamps`,
/// every file has a stamp (an old modification time bumped on each write).
struct Live {
    fs: Mutex<MemoryFs>,
    stamps: Option<Mutex<HashMap<String, FileStamp>>>,
    reads: Mutex<HashMap<String, usize>>,
    writes: AtomicUsize,
}

impl Live {
    fn new(stamps: bool) -> Live {
        let mut fs = MemoryFs::new();
        fs.set_cwd("/p");
        fs.add_file("/p/tsconfig.json", "{}");
        Live {
            fs: Mutex::new(fs),
            stamps: stamps.then(|| Mutex::new(HashMap::new())),
            reads: Mutex::new(HashMap::new()),
            writes: AtomicUsize::new(0),
        }
    }

    fn write(&self, path: &str, text: &str) {
        self.fs.lock().unwrap().add_file(path, text);
        let w = self.writes.fetch_add(1, Ordering::Relaxed) as u128;
        if let Some(s) = &self.stamps {
            // Seconds after the epoch: far older than any read, so never "racy".
            let stamp = FileStamp {
                len: text.len() as u64,
                modified_ns: (1 + w) * 1_000_000_000,
            };
            s.lock().unwrap().insert(path.to_owned(), stamp);
        }
    }

    fn reads(&self, path: &str) -> usize {
        self.reads.lock().unwrap().get(path).copied().unwrap_or(0)
    }
}

impl FileSystem for Live {
    fn read_file(&self, p: &str) -> Option<String> {
        *self.reads.lock().unwrap().entry(p.to_owned()).or_default() += 1;
        self.fs.lock().unwrap().read_file(p)
    }
    fn file_exists(&self, p: &str) -> bool {
        self.fs.lock().unwrap().file_exists(p)
    }
    fn directory_exists(&self, p: &str) -> bool {
        self.fs.lock().unwrap().directory_exists(p)
    }
    fn read_dir(&self, p: &str) -> Vec<DirEntry> {
        self.fs.lock().unwrap().read_dir(p)
    }
    fn realpath(&self, p: &str) -> String {
        self.fs.lock().unwrap().realpath(p)
    }
    fn case_sensitive(&self) -> bool {
        true
    }
    fn cwd(&self) -> String {
        "/p".to_owned()
    }
    fn stamp(&self, p: &str) -> Option<FileStamp> {
        self.stamps.as_ref()?.lock().unwrap().get(p).copied()
    }
}

const MAIN: &str = "import { is } from \"@ts-r.dev/guard\";\nimport type { U } from \"./types\";\nexport const f = (x: unknown) => is<U>(x);\n";

fn changed_file_is_reparsed(stamps: bool) {
    let live = Arc::new(Live::new(stamps));
    live.write("/p/main.ts", MAIN);
    live.write("/p/types.ts", "export interface U { a: number }\n");
    let ex = expander(live.clone());
    let first = ex.expand("/p/main.ts", MAIN).expect("expanded").code;
    assert!(first.contains("typeof v.a === \"number\""), "{first}");
    assert_eq!(ex.parsed_files(), 2);
    // Unchanged: nothing is parsed again (and with stamps, `types.ts` is not even read).
    let reads = live.reads("/p/types.ts");
    let again = ex.expand("/p/main.ts", MAIN).expect("expanded").code;
    assert_eq!(again, first);
    assert_eq!(ex.parsed_files(), 2);
    assert_eq!(
        live.reads("/p/types.ts") - reads,
        usize::from(!stamps),
        "reads of an unchanged file"
    );
    // Same length, different contents: re-parsed, and the validator follows it.
    live.write("/p/types.ts", "export interface U { a: string }\n");
    let second = ex.expand("/p/main.ts", MAIN).expect("expanded").code;
    assert!(second.contains("typeof v.a === \"string\""), "{second}");
    assert_eq!(ex.parsed_files(), 3, "only types.ts is parsed again");
    // `forget_files` drops a file's parsed copy (watch mode), `forget_configs` every one.
    ex.forget_files(&["/p/types.ts"]);
    ex.expand("/p/main.ts", MAIN).expect("expanded");
    assert_eq!(ex.parsed_files(), 4);
    ex.forget_configs();
    ex.expand("/p/main.ts", MAIN).expect("expanded");
    assert_eq!(ex.parsed_files(), 6);
}

#[test]
fn a_changed_file_is_reparsed_by_contents() {
    changed_file_is_reparsed(false);
}

#[test]
fn a_changed_file_is_reparsed_by_stamp() {
    changed_file_is_reparsed(true);
}

#[test]
fn a_file_rewritten_within_the_racy_window_is_read_again() {
    // Real files: the second write may keep the modification time (coarse clocks) and has
    // the same length, so only re-reading recently modified files catches it.
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("guard-cache-racy");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let root = ts_r_config::path::normalize_slashes(&dir.to_string_lossy());
    std::fs::write(format!("{root}/tsconfig.json"), "{}").expect("tsconfig");
    let types = format!("{root}/types.ts");
    std::fs::write(&types, "export interface U { a: number }\n").expect("types");
    let main = format!("{root}/main.ts");
    std::fs::write(&main, MAIN).expect("main");
    let ex = expander(Arc::new(ts_r_config::RealFs::new()));
    let first = ex.expand(&main, MAIN).expect("expanded").code;
    assert!(first.contains("typeof v.a === \"number\""), "{first}");
    std::fs::write(&types, "export interface U { a: string }\n").expect("types");
    let second = ex.expand(&main, MAIN).expect("expanded").code;
    assert!(second.contains("typeof v.a === \"string\""), "{second}");
    let _ = std::fs::remove_dir_all(&dir);
}
