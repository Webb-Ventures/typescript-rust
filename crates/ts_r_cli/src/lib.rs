//! The ts-r launcher: finds and runs the `ts-r-*` tool binaries. It contains no compiler code
//! and must start in well under 5 ms, so arguments are parsed by hand and nothing touches the
//! filesystem before it is needed.

pub mod ci;
pub mod doctor;
pub mod resolve;
pub mod tools;
pub mod version;

use std::ffi::OsString;
use std::path::Path;

use resolve::{Env, Lookup};
use version::{Origin, Selected, VERSION};

/// Exit code when a tool isn't installed (shell convention for "command not found").
pub const EXIT_NOT_FOUND: i32 = 127;
/// Exit code when a tool was found but could not be started.
pub const EXIT_CANNOT_RUN: i32 = 126;
/// Exit code for usage errors.
pub const EXIT_USAGE: i32 = 2;

/// `--help` text.
pub fn help() -> String {
    let mut s = format!(
        "ts-r {VERSION}: fast TypeScript tooling\n\nUsage: ts-r <command> [args...]\n\nCommands:\n"
    );
    for t in tools::TOOLS {
        s.push_str(&format!("  {:<8} {}  [{}]\n", t.name, t.about, t.milestone));
    }
    s.push_str(
        "  ci       Run check, lint and build in parallel\n\
         \x20 doctor   Show where tools are found, the cache, and the project setup\n\
         \nOptions:\n\
         \x20 -h, --help     Print help\n\
         \x20 -V, --version  Print the launcher version\n\
         \nArguments after a tool command are passed to `ts-r-<command>` unchanged.\n\
         A .ts-r-version file (nearest one above the current directory) or TS_R_VERSION\n\
         selects which version's tools run. Tools are looked up next to this launcher, then in\n\
         the cache (`ts-r doctor` shows where), then on PATH.\n",
    );
    s
}

/// Runs the launcher with `args` (without the program name). Returns the exit code, or never
/// returns when a tool is exec'd (Unix).
pub fn run(args: Vec<OsString>) -> i32 {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        print!("{}", help());
        return 0;
    };
    let rest: Vec<OsString> = args.collect();
    let command = first.to_str().unwrap_or("");
    match command {
        "-h" | "--help" | "help" => {
            print!("{}", help());
            0
        }
        "-V" | "--version" | "version" => {
            println!("ts-r {VERSION}");
            0
        }
        "ci" => with_version(|selected| ci::run(&rest, &selected, &Env::from_process())),
        "doctor" => with_version(|selected| {
            let cwd = std::env::current_dir().ok();
            print!(
                "{}",
                doctor::report(&selected, &Env::from_process(), cwd.as_deref())
            );
            0
        }),
        _ => match tools::find(command) {
            Some(tool) => with_version(|selected| run_tool(tool, &selected, rest)),
            None if command.starts_with('-') => {
                eprintln!("ts-r: unknown option {first:?}\nRun `ts-r --help` for usage.");
                EXIT_USAGE
            }
            None => {
                eprintln!(
                    "ts-r: unknown command {first:?}\nRun `ts-r --help` for the list of commands."
                );
                EXIT_USAGE
            }
        },
    }
}

fn with_version(f: impl FnOnce(Selected) -> i32) -> i32 {
    let cwd = std::env::current_dir().ok();
    let env_pin = std::env::var("TS_R_VERSION").ok();
    match version::select(cwd.as_deref(), env_pin.as_deref()) {
        Ok(selected) => f(selected),
        Err(e) => {
            eprintln!("ts-r: {e}");
            EXIT_USAGE
        }
    }
}

fn run_tool(tool: &tools::Tool, selected: &Selected, args: Vec<OsString>) -> i32 {
    let env = Env::from_process();
    match resolve::resolve(tool, selected, &env) {
        Lookup::Found { path, .. } => exec(&path, args),
        Lookup::Missing { searched } => {
            eprintln!("{}", missing_message(tool, selected, &searched));
            EXIT_NOT_FOUND
        }
    }
}

/// The error shown when a tool can't be found.
pub fn missing_message(tool: &tools::Tool, selected: &Selected, searched: &[String]) -> String {
    let name = tool.display_name();
    let mut msg = match &selected.origin {
        Origin::Launcher => {
            format!(
                "ts-r: {name} isn't installed; it ships in milestone {}.",
                tool.milestone
            )
        }
        Origin::Env => format!(
            "ts-r: {name} {} (from TS_R_VERSION) isn't installed.",
            selected.version
        ),
        Origin::File(f) => format!(
            "ts-r: {name} {} (pinned by {}) isn't installed.",
            selected.version,
            f.display()
        ),
    };
    msg.push_str("\n  looked in:");
    for s in searched {
        msg.push_str("\n    ");
        msg.push_str(s);
    }
    msg
}

/// Replaces this process with the tool (Unix) so signals, stdio and the exit code are the
/// tool's own; elsewhere runs it and forwards the exit code.
fn exec(path: &Path, args: Vec<OsString>) -> i32 {
    let mut cmd = std::process::Command::new(path);
    cmd.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        eprintln!("ts-r: failed to run {}: {err}", path.display());
        EXIT_CANNOT_RUN
    }
    #[cfg(not(unix))]
    {
        match cmd.status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(err) => {
                eprintln!("ts-r: failed to run {}: {err}", path.display());
                EXIT_CANNOT_RUN
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lists_every_command() {
        let h = help();
        for t in tools::TOOLS {
            assert!(h.contains(&format!("  {:<8} ", t.name)), "{}", t.name);
        }
        assert!(h.contains("  ci ") && h.contains("  doctor "));
    }

    #[test]
    fn missing_message_names_the_milestone() {
        let run = tools::find("run").unwrap();
        let own = Selected {
            version: VERSION.into(),
            origin: Origin::Launcher,
        };
        let m = missing_message(run, &own, &["/a".into(), "PATH".into()]);
        assert!(
            m.starts_with("ts-r: ts-r-run isn't installed; it ships in milestone M1."),
            "{m}"
        );
        assert!(m.contains("    /a\n    PATH"));
        let pinned = Selected {
            version: "9.9.9".into(),
            origin: Origin::File("/p/.ts-r-version".into()),
        };
        assert!(missing_message(run, &pinned, &[]).contains("9.9.9 (pinned by /p/.ts-r-version)"));
    }

    #[test]
    fn usage_errors() {
        assert_eq!(run(vec!["--bogus".into()]), EXIT_USAGE);
        assert_eq!(run(vec!["frobnicate".into()]), EXIT_USAGE);
        assert_eq!(run(vec!["--version".into()]), 0);
    }
}
