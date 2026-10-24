//! esbuild-style argument parsing: `--flag`, `--flag=value`, `--flag:key=value`.

use ts_r_bundler::{
    BuildOptions, EntryPoint, Feature, Format, Loader, Platform, SourceMapMode, parse_target,
};

pub const HELP: &str = "\
Usage: ts-r-build [options] [entry points]

Simple options:
  --bundle                  Bundle all dependencies into the output files
  --define:K=V              Substitute K with V while parsing
  --external:M              Exclude module M from the bundle (can use * wildcards)
  --format=...              Output format (iife | cjs | esm)
  --loader:X=L              Use loader L for .X files (js, jsx, ts, tsx, json, text,
                            file, css, empty, base64, dataurl, binary)
  --minify                  Minify the output (sets all --minify-* flags)
  --outdir=...              The output directory (for multiple entry points)
  --outfile=...             The output file (for one entry point)
  --packages=external       Exclude every package import from the bundle
  --platform=...            Platform target (browser | node | neutral)
  --sourcemap               Emit a source map
  --splitting               Enable code splitting (requires --format=esm)
  --target=...              Environment target (es2015..es2025, esnext, node18, chrome110,
                            ...); newer syntax is lowered
  --watch                   Rebuild when a source file changes

Advanced options:
  --banner:js=...           Text at the start of every JS output
  --chunk-names=...         Path template for shared chunks (default \"[name]-[hash]\")
  --conditions=...          Custom package.json exports conditions
  --entry-names=...         Path template for entry points (default \"[dir]/[name]\")
  --footer:js=...           Text at the end of every JS output
  --global-name=...         Name of the global for --format=iife exports
  --ignore-annotations      Ignore /*#__PURE__*/ and sideEffects
  --jsx=...                 transform | automatic | preserve
  --jsx-dev                 Use React's automatic runtime in development mode
  --jsx-factory=...         What to use for JSX instead of React.createElement
  --jsx-fragment=...        What to use for JSX instead of React.Fragment
  --jsx-import-source=...   Override the package name for the automatic runtime
  --log-level=...           verbose | debug | info | warning | error | silent
  --main-fields=...         Override the main file order in package.json
  --metafile=...            Write metadata about the build to a JSON file
  --minify-whitespace       Remove whitespace
  --minify-identifiers      Shorten identifiers
  --minify-syntax           Use equivalent but shorter syntax
  --no-cache                Do not use the persistent build cache
  --out-extension:.js=.mjs  Use a custom output extension
  --public-path=...         Prefix for file-loader URLs
  --resolve-extensions=...  Comma-separated list of implicit extensions
  --sourcemap=...           linked | external | inline | both
  --sources-content=false   Omit sourcesContent in source maps
  --supported:F=false       Treat syntax feature F (esbuild's names) as unsupported
  --tree-shaking=...        Force tree shaking on or off (true | false)
  --tsconfig=...            Use this tsconfig.json instead of the nearest one
  --threads=N               Worker threads (default: TS_R_THREADS or all cores, max 8)
  --version                 Print the version
";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Silent,
    Error,
    Warning,
    Info,
    Debug,
}

#[derive(Debug)]
pub struct Cli {
    pub options: BuildOptions,
    pub watch: bool,
    pub metafile: Option<String>,
    pub log_level: LogLevel,
    /// No `--outfile`/`--outdir`: print the output to stdout.
    pub stdout: bool,
}

#[derive(Debug)]
pub enum Parsed {
    Run(Box<Cli>),
    Help,
    Version,
}

fn bool_value(flag: &str, v: Option<&str>) -> Result<bool, String> {
    match v {
        None | Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(other) => Err(format!(
            "invalid value {other:?} for {flag} (expected true or false)"
        )),
    }
}

fn list(v: &str) -> Vec<String> {
    v.split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn parse(argv: &[String]) -> Result<Parsed, String> {
    let mut o = BuildOptions {
        persistent_cache: true,
        ..BuildOptions::default()
    };
    let mut watch = false;
    let mut metafile = None;
    let mut log_level = LogLevel::Info;
    let mut minify = false;
    for arg in argv {
        let Some(flag) = arg.strip_prefix("--") else {
            if arg.starts_with('-') && arg.len() > 1 {
                return Err(format!("invalid flag {arg:?}"));
            }
            // `name=path` sets the output name.
            let (name, path) = match arg.split_once('=') {
                Some((n, p)) if !n.contains('/') && !n.is_empty() => {
                    (Some(n.to_owned()), p.to_owned())
                }
                _ => (None, arg.clone()),
            };
            o.entry_points.push(EntryPoint { path, name });
            continue;
        };
        // `--name:key=value`
        if let Some((name, rest)) = flag.split_once(':')
            && !name.contains('=')
        {
            let (key, value) = match rest.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (rest, None),
            };
            match name {
                "external" => o.external.push(rest.to_owned()),
                "define" => {
                    let v = value.ok_or_else(|| format!("missing \"=\" in {arg:?}"))?;
                    o.define.push((key.to_owned(), v.to_owned()));
                }
                "loader" => {
                    let v = value.ok_or_else(|| format!("missing \"=\" in {arg:?}"))?;
                    let l = Loader::parse(v).ok_or_else(|| format!("invalid loader {v:?}"))?;
                    let ext = if key.starts_with('.') {
                        key.to_owned()
                    } else {
                        format!(".{key}")
                    };
                    o.loaders.insert(ext, l);
                }
                "banner" | "footer" => {
                    if key != "js" {
                        return Err(format!("--{name}:{key} is not supported (only js)"));
                    }
                    let v = value.unwrap_or("").to_owned();
                    if name == "banner" {
                        o.banner = Some(v);
                    } else {
                        o.footer = Some(v);
                    }
                }
                "supported" => {
                    let f = Feature::parse(key).ok_or_else(|| {
                        format!("{key:?} is not a valid feature name for the \"supported\" setting")
                    })?;
                    o.supported.push((f, bool_value(name, value)?));
                }
                "out-extension" => {
                    let v = value.ok_or_else(|| format!("missing \"=\" in {arg:?}"))?;
                    if key == ".js" {
                        o.out_extension_js = v.to_owned();
                    }
                }
                _ => return Err(format!("invalid flag \"--{name}:\"")),
            }
            continue;
        }
        let (name, value) = match flag.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (flag, None),
        };
        let need = || value.ok_or_else(|| format!("missing value for --{name}"));
        match name {
            "help" | "h" => return Ok(Parsed::Help),
            "version" => return Ok(Parsed::Version),
            "bundle" => o.bundle = bool_value(name, value)?,
            "outdir" => o.outdir = Some(need()?.to_owned()),
            "outfile" => o.outfile = Some(need()?.to_owned()),
            "format" => {
                let v = need()?;
                o.format = Some(Format::parse(v).ok_or_else(|| format!("invalid format {v:?}"))?);
            }
            "platform" => {
                let v = need()?;
                o.platform = Platform::parse(v).ok_or_else(|| format!("invalid platform {v:?}"))?;
            }
            "target" => o.target = parse_target(need()?)?,
            "minify" => minify = bool_value(name, value)?,
            "minify-whitespace" => o.minify_whitespace = bool_value(name, value)?,
            "minify-identifiers" => o.minify_identifiers = bool_value(name, value)?,
            "minify-syntax" => o.minify_syntax = bool_value(name, value)?,
            "sourcemap" => {
                o.sourcemap = match value {
                    None | Some("true") | Some("linked") => SourceMapMode::Linked,
                    Some("external") => SourceMapMode::External,
                    Some("inline") => SourceMapMode::Inline,
                    Some("both") => SourceMapMode::Both,
                    Some("false") => SourceMapMode::None,
                    Some(v) => return Err(format!("invalid sourcemap value {v:?}")),
                }
            }
            "sources-content" => o.sources_content = bool_value(name, value)?,
            "splitting" => o.splitting = bool_value(name, value)?,
            "packages" => match need()? {
                "external" => o.packages_external = true,
                "bundle" => o.packages_external = false,
                v => return Err(format!("invalid --packages value {v:?}")),
            },
            "metafile" => {
                metafile = Some(need()?.to_owned());
                o.metafile = true;
            }
            "global-name" => o.global_name = Some(need()?.to_owned()),
            "main-fields" => o.main_fields = Some(list(need()?)),
            "conditions" => o.conditions = Some(list(need()?)),
            "resolve-extensions" => o.resolve_extensions = list(need()?),
            "tsconfig" => o.tsconfig = Some(need()?.to_owned()),
            "jsx" => {
                let v = need()?;
                if !matches!(v, "transform" | "automatic" | "preserve") {
                    return Err(format!("invalid --jsx value {v:?}"));
                }
                o.jsx.mode = Some(v.to_owned());
            }
            "jsx-dev" => o.jsx.dev = bool_value(name, value)?,
            "jsx-factory" => o.jsx.factory = Some(need()?.to_owned()),
            "jsx-fragment" => o.jsx.fragment = Some(need()?.to_owned()),
            "jsx-import-source" => o.jsx.import_source = Some(need()?.to_owned()),
            "entry-names" => o.entry_names = need()?.to_owned(),
            "chunk-names" => o.chunk_names = need()?.to_owned(),
            "asset-names" => o.asset_names = need()?.to_owned(),
            "public-path" => o.public_path = need()?.to_owned(),
            "tree-shaking" => o.tree_shaking = Some(bool_value(name, value)?),
            "ignore-annotations" => o.ignore_annotations = bool_value(name, value)?,
            "watch" => watch = bool_value(name, value)?,
            "no-cache" => o.persistent_cache = false,
            "threads" => {
                o.threads = need()?
                    .parse()
                    .map_err(|_| format!("invalid --threads value {value:?}"))?
            }
            "log-level" => {
                log_level = match need()? {
                    "silent" => LogLevel::Silent,
                    "error" => LogLevel::Error,
                    "warning" => LogLevel::Warning,
                    "info" => LogLevel::Info,
                    "debug" | "verbose" => LogLevel::Debug,
                    v => return Err(format!("invalid log level {v:?}")),
                }
            }
            "legal-comments" => {
                o.legal_comments_inline = match need()? {
                    "none" => false,
                    "inline" | "eof" => true,
                    v => return Err(format!("unsupported --legal-comments value {v:?}")),
                }
            }
            // Accepted for compatibility; they do not change our output.
            "allow-overwrite" | "charset" | "color" | "preserve-symlinks" => {}
            _ => return Err(format!("invalid flag \"--{name}\"")),
        }
    }
    if minify {
        o.minify_whitespace = true;
        o.minify_identifiers = true;
        o.minify_syntax = true;
    }
    let stdout = o.outfile.is_none() && o.outdir.is_none();
    if stdout {
        if o.entry_points.len() > 1 {
            return Err("must use \"outdir\" when there are multiple input files".into());
        }
        if watch {
            return Err("--watch requires --outfile or --outdir".into());
        }
        o.write = false;
    }
    if o.entry_points.is_empty() {
        return Ok(Parsed::Help);
    }
    Ok(Parsed::Run(Box::new(Cli {
        options: o,
        watch,
        metafile,
        log_level,
        stdout,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Cli {
        let v: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        match parse(&v) {
            Ok(Parsed::Run(c)) => *c,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn esbuild_flags() {
        let c = run(&[
            "src/a.ts",
            "lib=src/b.ts",
            "--bundle",
            "--outdir=out",
            "--format=esm",
            "--platform=node",
            "--target=es2022,node20",
            "--minify",
            "--sourcemap=external",
            "--splitting",
            "--external:react",
            "--external:@scope/*",
            "--define:process.env.NODE_ENV=\"production\"",
            "--loader:.png=file",
            "--metafile=meta.json",
            "--banner:js=// hi",
        ]);
        let o = &c.options;
        assert!(o.bundle && o.splitting && o.minify_identifiers && o.minify_syntax);
        assert_eq!(o.entry_points[1].name.as_deref(), Some("lib"));
        assert_eq!(o.format, Some(Format::Esm));
        assert_eq!(o.platform, Platform::Node);
        assert_eq!(o.target, Some(2022));
        assert_eq!(o.sourcemap, SourceMapMode::External);
        assert_eq!(o.external, vec!["react", "@scope/*"]);
        assert_eq!(
            o.define[0],
            ("process.env.NODE_ENV".into(), "\"production\"".into())
        );
        assert_eq!(o.loaders.get(".png"), Some(&Loader::File));
        assert_eq!(c.metafile.as_deref(), Some("meta.json"));
        assert_eq!(o.banner.as_deref(), Some("// hi"));
        assert!(!c.stdout);
    }

    #[test]
    fn errors_and_stdout() {
        let v = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(parse(&v(&["a.js", "--nope"])).is_err());
        assert!(parse(&v(&["a.js", "--format=amd"])).is_err());
        assert!(parse(&v(&["a.js", "b.js"])).is_err());
        let c = run(&["a.js", "--bundle"]);
        assert!(c.stdout && !c.options.write);
    }
}
