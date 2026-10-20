//! Regressions found on real projects (private-app) and in review, outside the design-record
//! cases.
//!
//! Skipped when the lib files are missing (`cargo xtask upstream sync --minimal`).

use ts_r_checker::{CheckRun, LibStore, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_diagnostics::code_label;
use ts_r_sound::config::{Level, SoundConfig};
use ts_r_sound::{SoundRun, check_sound};

ts_r_testing::cap_memory!();

/// `(file, one-based line, code)` of every diagnostic of a strict project whose files are all
/// at `sound`.
fn check(files: &[(&str, &str)]) -> Option<Vec<(String, u32, String)>> {
    let libs = LibStore::locate()?;
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions":{"strict":true,"noEmit":true},"include":["*.ts"]}"#,
    );
    for (name, text) in files {
        fs.add_file(&format!("/p/{name}"), text);
    }
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let base = check_program(&program, CheckRun { threads: 1 });
    let sound_cfg = SoundConfig::everything_at(Level::Sound, "/p");
    let sound = check_sound(&SoundRun {
        program: &program,
        config: &sound_cfg,
        base: &base.diagnostics,
        threads: 1,
        case_sensitive: true,
    });
    let mut out = Vec::new();
    for d in base.diagnostics.iter().chain(&sound.diagnostics) {
        let Some(f) = program.files.get(d.file.0 as usize) else {
            continue;
        };
        if f.is_lib {
            continue;
        }
        let line = f.lines().line_of(d.span.start) + 1;
        out.push((
            f.path.trim_start_matches("/p/").to_owned(),
            line,
            code_label(d.code),
        ));
    }
    out.sort();
    Some(out)
}

/// S-01 sees a foreign `any` as `unknown` deeply: a `.d.ts` `{ id: any }` (or `Promise<any>`)
/// assigned to a typed target in a sound file is a TSR0102, not only a bare `any`.
#[test]
fn foreign_any_nested_in_a_typed_assignment() {
    let foreign = "\
export declare function load(): { id: any };
export declare function list(): any[];
export declare const meta: { readonly version: string; readonly extra: any };
export declare function wrap(): { inner: { tags: any[] } } | null;
";
    let user = "\
import { load, list, meta, wrap } from './foreign';
export const user: { id: number } = load();
export const ids: number[] = list();
export const info: { version: string; extra: unknown } = meta;
export const loose: { id: unknown } = load();
export const w: { inner: { tags: string[] } } | null = wrap();
";
    let Some(got) = check(&[("foreign.d.ts", foreign), ("user.ts", user)]) else {
        eprintln!("skipped: lib files not found");
        return;
    };
    assert_eq!(
        got,
        vec![
            ("user.ts".to_owned(), 2, "TSR0102".to_owned()),
            ("user.ts".to_owned(), 3, "TSR0102".to_owned()),
            ("user.ts".to_owned(), 6, "TSR0102".to_owned()),
        ]
    );
}

/// TSR1003 also for a non-async callback whose body returns a promise.
#[test]
fn promise_returning_non_async_callback_for_a_void_parameter() {
    let api = "export declare function register(cb: () => void): void;\n";
    let src = "\
import { register } from './api';
register(() => Promise.reject(new Error('x')));
register(function () { return Promise.resolve(1); });
register(() => { console.log('x'); });
";
    let Some(got) = check(&[("api.d.ts", api), ("cb.ts", src)]) else {
        eprintln!("skipped: lib files not found");
        return;
    };
    assert_eq!(
        got,
        vec![
            ("cb.ts".to_owned(), 2, "TSR1003".to_owned()),
            ("cb.ts".to_owned(), 3, "TSR1003".to_owned()),
        ]
    );
}
