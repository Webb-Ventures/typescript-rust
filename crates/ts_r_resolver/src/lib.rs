//! Node and TypeScript module resolution, matching tsc 7 (TypeScript 7.0.2).
//!
//! - `moduleResolution` `node16`, `nodenext` and `bundler` (TS 7 removed `node10` and
//!   `classic`; they run as `bundler`).
//! - Relative and absolute specifiers with extension substitution (`.js` → `.ts`), directory
//!   modules and `moduleSuffixes`; ESM-mode imports under node16/nodenext need full paths.
//! - `package.json` `main`/`types`/`typings`/`typesVersions`, `exports` and `imports`
//!   (conditions, `*` patterns, `null` targets, versioned `types@<range>`), self-references,
//!   `@types` packages, `paths`, `rootDirs`, and symlink real paths (pnpm layouts).
//! - Type reference directives with type roots.
//!
//! ```
//! use ts_r_config::{MemoryFs, load_tsconfig};
//! use ts_r_resolver::{ResolutionMode, Resolver, ResolverOptions};
//!
//! let mut fs = MemoryFs::new();
//! fs.add_file("/p/tsconfig.json", r#"{ "compilerOptions": { "module": "nodenext" } }"#)
//!     .add_file("/p/src/a.ts", "")
//!     .add_file("/p/src/b.ts", "")
//!     .add_file("/p/node_modules/lib/package.json", r#"{ "exports": { "types": "./i.d.ts" } }"#)
//!     .add_file("/p/node_modules/lib/i.d.ts", "");
//! let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
//! let resolver = Resolver::new(&fs, ResolverOptions::from_compiler_options(&cfg.options));
//! let b = resolver.resolve_module("./b.js", "/p/src/a.ts", ResolutionMode::Import).unwrap();
//! assert_eq!(b.resolved_file_name, "/p/src/b.ts");
//! let lib = resolver.resolve_module("lib", "/p/src/a.ts", ResolutionMode::Require).unwrap();
//! assert_eq!(lib.resolved_file_name, "/p/node_modules/lib/i.d.ts");
//! assert!(lib.is_external_library_import);
//! ```

pub mod options;
pub mod package_json;
pub mod references;
pub mod resolver;
pub mod semver;
pub mod trace;

pub use options::ResolverOptions;
pub use package_json::PackageJson;
pub use references::ProjectReferences;
pub use resolver::{
    AmbiguousRoot, Extension, ImportKind, ModuleFormat, PackageId, ResolutionMode, ResolvedModule,
    ResolvedTypeReference, Resolver, TS_VERSION, mangle_scoped_name, split_package_name,
};
pub use trace::Trace;
