//! `ts-r-lsp`: see `--help`.

use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::ExitCode;
use ts_r_lsp::backend::ProcessFactory;
use ts_r_lsp::front::{self, FrontOptions};
use ts_r_lsp::{daemon, fake, ipc};

const HELP: &str = "\
ts-r-lsp: one shared, memory-budgeted language server per workspace

USAGE
  ts-r-lsp [--stdio] [OPTIONS]          LSP server on stdio (spawns or attaches to the daemon)
  ts-r-lsp tsserver [TSSERVER ARGS]     tsserver stand-in on stdio (same daemon)
  ts-r-lsp [NODE FLAGS] <path>/tsserver.js [TSSERVER ARGS]
                                        same, when set as VS Code's typescript.tsserver.nodePath
  ts-r-lsp status [--root DIR]          print the workspace daemon's status as JSON
  ts-r-lsp stop [--root DIR]            ask the workspace daemon to exit
  ts-r-lsp ensure-daemon [--root DIR]   start the daemon if needed and print its address
  ts-r-lsp daemon --root DIR            run the daemon in the foreground

OPTIONS
  --root DIR                    workspace root (default: from initialize, else the cwd)
  --memory-budget-mb N          ts-r.memoryBudgetMB (default 1500)
  --idle-shutdown-minutes N     ts-r.idleShutdownMinutes (default 10, 0 = never)
  --type-backend ts-r|typescript  ts-r.typeBackend (ts-r: our checker, LSP clients only)
  --lint auto|on|off            ts-r.lint.enable for this front (default auto, which lints)
  --lint-only                   lint diagnostics and code actions only, no type backend
  --fallback-tsserver PATH      tsserver to use when the project has none
  --node PATH                   node binary for tsserver when node is not on PATH
  --name NAME                   name shown in status and logs
  --version, --help
";

struct Parsed {
    command: String,
    opts: FrontOptions,
    rest: Vec<String>,
}

fn parse(args: Vec<String>) -> Result<Parsed, String> {
    let mut opts = FrontOptions {
        settings: json!({}),
        ..Default::default()
    };
    let mut command = String::from("lsp");
    let mut rest = Vec::new();
    let mut it = args.into_iter();
    let mut settings = serde_json::Map::new();
    while let Some(a) = it.next() {
        let mut value = |name: &str| next_value(&mut it, name);
        match a.as_str() {
            "--stdio" => {}
            "--root" => opts.root = Some(PathBuf::from(value("--root")?)),
            "--memory-budget-mb" => {
                let v = value("--memory-budget-mb")?;
                settings.insert(
                    "memoryBudgetMB".into(),
                    json!(v.parse::<u64>().map_err(|_| "bad --memory-budget-mb")?),
                );
            }
            "--idle-shutdown-minutes" => {
                let v = value("--idle-shutdown-minutes")?;
                settings.insert(
                    "idleShutdownMinutes".into(),
                    json!(
                        v.parse::<f64>()
                            .map_err(|_| "bad --idle-shutdown-minutes")?
                    ),
                );
            }
            "--type-backend" => {
                settings.insert(
                    "typeBackend".into(),
                    Value::String(value("--type-backend")?),
                );
            }
            "--lint" => {
                settings.insert("lint".into(), Value::String(value("--lint")?));
            }
            "--lint-only" => {
                settings.insert("lintOnly".into(), Value::Bool(true));
            }
            "--fallback-tsserver" => opts.fallback_tsserver = Some(value("--fallback-tsserver")?),
            "--node" => opts.node_path = Some(value("--node")?),
            "--name" => opts.name = Some(value("--name")?),
            "status" | "stop" | "ensure-daemon" | "daemon" | "tsserver" | "__fake-backend"
                if command == "lsp" =>
            {
                command = a;
                if command == "tsserver" || command == "__fake-backend" {
                    rest.extend(it.by_ref());
                }
            }
            _ if a.ends_with(".js") && command == "lsp" => {
                // Started as `typescript.tsserver.nodePath <node flags> <tsserver.js> <args>`.
                command = "tsserver".into();
                opts.fallback_tsserver.get_or_insert(a);
                rest.extend(it.by_ref());
            }
            _ if a.starts_with("--") && command == "lsp" && is_node_flag(&a) => {
                // Node flags before the script path (e.g. --max-old-space-size=3072).
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    opts.settings = Value::Object(settings);
    Ok(Parsed {
        command,
        opts,
        rest,
    })
}

fn next_value(it: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{name} needs a value"))
}

/// Node flags VS Code may pass before the script (`--max-old-space-size=N`, `--inspect...`).
fn is_node_flag(a: &str) -> bool {
    a.starts_with("--max-old-space-size")
        || a.starts_with("--inspect")
        || a.starts_with("--heap")
        || a.starts_with("--expose")
        || a.starts_with("--diagnostic")
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") && !args.iter().any(|a| a.ends_with(".js")) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    if args.first().is_some_and(|a| a == "--version" || a == "-v") {
        println!("ts-r-lsp {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let p = match parse(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ts-r-lsp: {e}\n\n{HELP}");
            return ExitCode::from(2);
        }
    };
    let root = || {
        p.opts
            .root
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    };
    let code = match p.command.as_str() {
        "lsp" => front::run_lsp_front(p.opts.clone()),
        "tsserver" => front::run_tsserver_front(&p.rest, p.opts.clone()),
        "daemon" => match daemon::serve(&root(), Box::new(ProcessFactory)) {
            Ok(true) => 0,
            Ok(false) => {
                eprintln!("ts-r-lsp: a daemon already serves {}", root().display());
                0
            }
            Err(e) => {
                eprintln!("ts-r-lsp: daemon failed: {e}");
                1
            }
        },
        "ensure-daemon" => {
            let ep = ipc::Endpoint::for_root(&root());
            match std::env::current_exe().and_then(|exe| ipc::ensure_daemon(&ep, &exe)) {
                Ok(_) => {
                    println!("{}", ep.address);
                    0
                }
                Err(e) => {
                    eprintln!("ts-r-lsp: {e}");
                    1
                }
            }
        }
        "status" => match front::status(&root()) {
            Ok(s) => {
                println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default());
                0
            }
            Err(e) => {
                eprintln!("ts-r-lsp: no daemon for {} ({e})", root().display());
                1
            }
        },
        "stop" => match front::stop(&root()) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("ts-r-lsp: no daemon for {} ({e})", root().display());
                1
            }
        },
        "__fake-backend" => match p.rest.first().map(String::as_str) {
            Some("tsserver") => fake::run_fake_tsserver(&p.rest[1..]),
            Some("lsp") => fake::run_fake_lsp(),
            _ => 2,
        },
        _ => 2,
    };
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn parses_lsp_options() {
        let p = parse(args(&[
            "--stdio",
            "--root",
            "/w",
            "--memory-budget-mb",
            "900",
        ]))
        .unwrap();
        assert_eq!(p.command, "lsp");
        assert_eq!(p.opts.root, Some(PathBuf::from("/w")));
        assert_eq!(p.opts.settings["memoryBudgetMB"], 900);
        assert!(parse(args(&["--memory-budget-mb", "x"])).is_err());
        assert!(parse(args(&["--bogus"])).is_err());
    }

    #[test]
    fn parses_node_path_invocation() {
        let p = parse(args(&[
            "--max-old-space-size=3072",
            "/vscode/node_modules/typescript/lib/tsserver.js",
            "--useInferredProjectPerProjectRoot",
            "--cancellationPipeName",
            "/tmp/x*",
        ]))
        .unwrap();
        assert_eq!(p.command, "tsserver");
        assert_eq!(
            p.opts.fallback_tsserver.as_deref(),
            Some("/vscode/node_modules/typescript/lib/tsserver.js")
        );
        assert_eq!(p.rest.len(), 3);
    }

    #[test]
    fn parses_subcommands() {
        assert_eq!(
            parse(args(&["status", "--root", "/w"])).unwrap().command,
            "status"
        );
        let p = parse(args(&["tsserver", "--serverMode", "partialSemantic"])).unwrap();
        assert_eq!(p.rest, args(&["--serverMode", "partialSemantic"]));
    }
}
