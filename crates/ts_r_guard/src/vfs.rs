//! [`GuardFs`]: a [`FileSystem`] for the bundler that expands `parse<T>` call sites as files
//! are read and serves the validators as virtual modules (record 0007, "Bundling and tree
//! shaking").
//!
//! Virtual modules live under `<project root>/.ts-r-guard/`, a directory that exists only
//! here. It mirrors the project: `<vroot>/<file>/<Name>.js` is the validator module of type
//! `Name` declared in `<project root>/<file>` (`<vroot>/<file>/~mod~<module>/<Name>.js` for
//! a declaration in a `declare module "<module>"` block, `<vroot>/~abs/<path>/...` for files
//! outside the root), and `<vroot>/rt.js` is the runtime. A module's path is the
//! declaration's identity, so every call site of a bundle shares one copy, and its text is
//! regenerated from the declarations whenever it is read (the bundler's content-hashed
//! caches then see changes). Directory listings are complete without knowing which modules
//! will be asked for (a declaration file lists every type alias and interface it declares),
//! so listing caches never miss a module.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use ts_r_ast::Node;
use ts_r_config::fs::{DirEntry, EntryKind};
use ts_r_config::{FileSystem, path};

use crate::Generator;
use crate::expand::{self, ExpandOptions, Expander, GuardError, Layout};

/// The virtual directory's name under the project root.
pub const VDIR: &str = ".ts-r-guard";

/// A file's guard errors.
#[derive(Clone, Debug)]
pub struct FileErrors {
    pub path: String,
    pub errors: Vec<GuardError>,
}

pub struct GuardFs {
    inner: Arc<dyn FileSystem>,
    expander: Expander,
    project_root: String,
    vroot: String,
    /// The errors of each file's last read, kept until it is read again: a rebuild that
    /// reuses a module without reading it still reports them.
    errors: Mutex<BTreeMap<String, Vec<GuardError>>>,
    /// Files expanded and the declaration files they depend on (for watchers).
    touched: Mutex<BTreeSet<String>>,
}

impl std::fmt::Debug for GuardFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardFs")
            .field("vroot", &self.vroot)
            .finish_non_exhaustive()
    }
}

/// What a virtual path names.
enum Virtual {
    Runtime,
    /// A directory mirroring `real` (a directory, or a declaration file listing its types).
    Dir {
        real: String,
        module: Option<String>,
    },
    Decl {
        file: String,
        module: Option<String>,
        name: String,
    },
}

fn is_decl_source(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    [".ts", ".tsx", ".mts", ".cts"]
        .iter()
        .any(|e| l.ends_with(e))
}

impl GuardFs {
    /// `layout_shared`: serve virtual modules (bundles); otherwise every file inlines its
    /// validators (unbundled output).
    pub fn new(
        inner: Arc<dyn FileSystem>,
        working_dir: &str,
        guard: crate::GuardOptions,
        layout_shared: bool,
    ) -> GuardFs {
        let real = inner.realpath(working_dir);
        let project_root = if real.is_empty() {
            path::normalize(working_dir)
        } else {
            path::normalize(&real)
        };
        let vroot = format!("{}/{VDIR}", project_root.trim_end_matches('/'));
        let layout = if layout_shared {
            Layout::Shared {
                project_root: project_root.clone(),
                vroot: vroot.clone(),
            }
        } else {
            Layout::Inline
        };
        GuardFs {
            expander: Expander::new(inner.clone(), ExpandOptions { guard, layout }),
            inner,
            project_root,
            vroot,
            errors: Mutex::new(BTreeMap::new()),
            touched: Mutex::new(BTreeSet::new()),
        }
    }

    /// Resolves every file with the compiler options of `tsconfig` (the bundler's
    /// `--tsconfig`) instead of each file's nearest one.
    pub fn with_tsconfig(self, tsconfig: &str) -> GuardFs {
        GuardFs {
            expander: self.expander.with_tsconfig(tsconfig),
            ..self
        }
    }

    /// A config file changed: tsconfig options are reloaded on the next read (and every file
    /// re-parsed).
    pub fn forget_configs(&self) {
        self.expander.forget_configs();
    }

    /// Files changed or were deleted (watch): their parsed copies are dropped.
    pub fn forget_files<S: AsRef<str>>(&self, paths: &[S]) {
        self.expander.forget_files(paths);
    }

    /// Files parsed so far for expansions and validator modules.
    pub fn parsed_files(&self) -> usize {
        self.expander.parsed_files()
    }

    pub fn vroot(&self) -> &str {
        &self.vroot
    }

    /// The errors of the last read of each of `files` (a build's inputs), sorted by path.
    pub fn errors_in(&self, files: &[String]) -> Vec<FileErrors> {
        let errors = self.errors.lock().unwrap_or_else(|e| e.into_inner());
        let files: BTreeSet<&str> = files.iter().map(String::as_str).collect();
        errors
            .iter()
            .filter(|(p, _)| files.contains(p.as_str()))
            .map(|(p, e)| FileErrors {
                path: p.clone(),
                errors: e.clone(),
            })
            .collect()
    }

    /// Expanded files and their declaration dependencies seen so far.
    pub fn touched(&self) -> Vec<String> {
        self.touched
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    fn classify(&self, p: &str) -> Option<Virtual> {
        let rest = p.strip_prefix(&self.vroot)?;
        if rest.is_empty() {
            return Some(Virtual::Dir {
                real: self.project_root.clone(),
                module: None,
            });
        }
        let rest = rest.strip_prefix('/')?;
        if rest == "rt.js" {
            return Some(Virtual::Runtime);
        }
        let mut segs: Vec<&str> = rest.split('/').collect();
        let abs = segs.first() == Some(&"~abs");
        if abs {
            segs.remove(0);
        }
        let base = if abs {
            String::new()
        } else {
            self.project_root.trim_end_matches('/').to_owned()
        };
        // Walk segments: once a segment names a source file, the rest is
        // `[~mod~<m>/]<Name>.js`.
        let mut real = base;
        let mut i = 0;
        while i < segs.len() {
            let s = segs[i];
            if s.is_empty() || s == "." || s == ".." {
                return None;
            }
            real = format!("{real}/{s}");
            i += 1;
            if is_decl_source(s) && self.inner.file_exists(&real) {
                let tail = &segs[i..];
                return match tail {
                    [] => Some(Virtual::Dir { real, module: None }),
                    [m] if m.starts_with("~mod~") => Some(Virtual::Dir {
                        real,
                        module: Some(expand::dec(&m["~mod~".len()..])),
                    }),
                    [n] => n.strip_suffix(".js").map(|n| Virtual::Decl {
                        file: real.clone(),
                        module: None,
                        name: n.to_owned(),
                    }),
                    [m, n] if m.starts_with("~mod~") => {
                        n.strip_suffix(".js").map(|n| Virtual::Decl {
                            file: real.clone(),
                            module: Some(expand::dec(&m["~mod~".len()..])),
                            name: n.to_owned(),
                        })
                    }
                    _ => None,
                };
            }
        }
        if real.is_empty() {
            real.push('/');
        }
        self.inner
            .directory_exists(&real)
            .then_some(Virtual::Dir { real, module: None })
    }

    /// Type aliases and interfaces of `file` (in block `module`), and its ambient blocks.
    fn decl_listing(&self, file: &str, module: Option<&str>) -> Vec<DirEntry> {
        let Some(text) = self.inner.read_file(file) else {
            return Vec::new();
        };
        let kind = ts_r_ast::SourceKind::from_path(file);
        let parsed = ts_r_parser::parse(&text, kind);
        let ast = &parsed.ast;
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return Vec::new();
        };
        let ident = |n| match *ast.node(n) {
            Node::Ident { name } => Some(ast.atom(name).to_owned()),
            Node::StringLit { value } => Some(ast.atom(value).to_owned()),
            _ => None,
        };
        let mut stmts = ast.list(stmts).to_vec();
        let mut out: BTreeSet<(String, bool)> = BTreeSet::new();
        if let Some(m) = module {
            let block = stmts.iter().find_map(|&st| match *ast.node(st) {
                Node::Module {
                    name,
                    body: Some(b),
                    ..
                } if ident(name).as_deref() == Some(m) => match *ast.node(b) {
                    Node::ModuleBlock { stmts } => Some(ast.list(stmts).to_vec()),
                    _ => None,
                },
                _ => None,
            });
            stmts = block.unwrap_or_default();
        }
        let mut entries = Vec::new();
        for st in stmts {
            match *ast.node(st) {
                Node::TypeAlias { name, .. } | Node::Interface { name, .. } => {
                    if let Some(n) = ident(name) {
                        out.insert((format!("{n}.js"), false));
                    }
                }
                Node::Module {
                    name,
                    body: Some(_),
                    ..
                } if module.is_none() && matches!(*ast.node(name), Node::StringLit { .. }) => {
                    if let Some(n) = ident(name) {
                        out.insert((format!("~mod~{}", enc_seg(&n)), true));
                    }
                }
                _ => {}
            }
        }
        for (name, dir) in out {
            let kind = if dir {
                EntryKind::Directory
            } else {
                EntryKind::File
            };
            entries.push(DirEntry { name, kind });
        }
        entries
    }

    fn decl_module(&self, file: &str, module: Option<&str>, name: &str) -> String {
        let text = self.inner.read_file(file).unwrap_or_default();
        let program = self.expander.program(file, &text);
        let Some(idx) = program.file_index(file) else {
            return format!(
                "throw new Error({:?});\n",
                format!("ts-r guard: cannot load {file}")
            );
        };
        let mut g = Generator::new(&program, self.expander.opts.guard.clone());
        let (root, vroot) = match &self.expander.opts.layout {
            Layout::Shared {
                project_root,
                vroot,
            } => (project_root.clone(), vroot.clone()),
            Layout::Inline => (self.project_root.clone(), self.vroot.clone()),
        };
        match g.decl_module(idx, module, name, &root, &vroot) {
            Ok(code) => code,
            Err(e) => format!(
                "throw new Error({:?});\n",
                format!("ts-r guard: no validator for '{name}' in {file}: {e}")
            ),
        }
    }
}

fn enc_seg(s: &str) -> String {
    s.replace('%', "%25").replace('/', "%2F")
}

impl FileSystem for GuardFs {
    fn read_file(&self, p: &str) -> Option<String> {
        if let Some(v) = self.classify(p) {
            return match v {
                Virtual::Runtime => Some(expand::runtime_module(&self.expander.opts.guard)),
                Virtual::Decl { file, module, name } => {
                    self.touched
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(file.clone());
                    Some(self.decl_module(&file, module.as_deref(), &name))
                }
                Virtual::Dir { .. } => None,
            };
        }
        let text = self.inner.read_file(p)?;
        self.errors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(p);
        let Some(x) = self.expander.expand(p, &text) else {
            return Some(text);
        };
        {
            let mut t = self.touched.lock().unwrap_or_else(|e| e.into_inner());
            t.insert(p.to_owned());
            t.extend(x.deps.iter().cloned());
        }
        if !x.errors.is_empty() {
            self.errors
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(p.to_owned(), x.errors);
        }
        Some(x.code)
    }

    fn file_exists(&self, p: &str) -> bool {
        match self.classify(p) {
            Some(Virtual::Runtime) => true,
            Some(Virtual::Decl { file, module, name }) => self
                .decl_listing(&file, module.as_deref())
                .iter()
                .any(|e| e.name == format!("{name}.js")),
            Some(Virtual::Dir { .. }) => false,
            None => self.inner.file_exists(p),
        }
    }

    fn directory_exists(&self, p: &str) -> bool {
        match self.classify(p) {
            Some(Virtual::Dir { .. }) => true,
            Some(_) => false,
            None => self.inner.directory_exists(p),
        }
    }

    fn read_dir(&self, p: &str) -> Vec<DirEntry> {
        match self.classify(p) {
            Some(Virtual::Dir { real, module }) => {
                if is_decl_source(&real) && self.inner.file_exists(&real) {
                    return self.decl_listing(&real, module.as_deref());
                }
                let mut out: Vec<DirEntry> = self
                    .inner
                    .read_dir(&real)
                    .into_iter()
                    .filter_map(|e| match e.kind {
                        EntryKind::Directory if e.name != VDIR => Some(DirEntry {
                            name: e.name,
                            kind: EntryKind::Directory,
                        }),
                        EntryKind::File if is_decl_source(&e.name) => Some(DirEntry {
                            name: e.name,
                            kind: EntryKind::Directory,
                        }),
                        _ => None,
                    })
                    .collect();
                if p == self.vroot {
                    out.push(DirEntry {
                        name: "rt.js".to_owned(),
                        kind: EntryKind::File,
                    });
                    out.push(DirEntry {
                        name: "~abs".to_owned(),
                        kind: EntryKind::Directory,
                    });
                }
                out.sort_by(|a, b| a.name.cmp(&b.name));
                out
            }
            Some(_) => Vec::new(),
            None => {
                let mut out = self.inner.read_dir(p);
                if path::normalize(p).trim_end_matches('/')
                    == self.project_root.trim_end_matches('/')
                    && !out.iter().any(|e| e.name == VDIR)
                {
                    out.push(DirEntry {
                        name: VDIR.to_owned(),
                        kind: EntryKind::Directory,
                    });
                    out.sort_by(|a, b| a.name.cmp(&b.name));
                }
                out
            }
        }
    }

    fn realpath(&self, p: &str) -> String {
        if p.starts_with(&self.vroot) {
            p.to_owned()
        } else {
            self.inner.realpath(p)
        }
    }

    fn case_sensitive(&self) -> bool {
        self.inner.case_sensitive()
    }

    fn cwd(&self) -> String {
        self.inner.cwd()
    }
}
