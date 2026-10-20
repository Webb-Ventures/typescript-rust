//! tsconfig.json loading: JSONC, `extends` chains, compiler options, file discovery.
//!
//! Matches tsc 7 (TypeScript 7.0.2) behaviour, verified against `tsc --showConfig`.
//!
//! Typical use:
//!
//! ```
//! use ts_r_config::{MemoryFs, load_tsconfig};
//! let mut fs = MemoryFs::new();
//! fs.add_file("/p/tsconfig.json", r#"{ "compilerOptions": { "module": "nodenext" } }"#)
//!   .add_file("/p/src/a.ts", "export {}");
//! let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
//! assert!(cfg.diagnostics.is_empty());
//! assert_eq!(cfg.file_names, ["/p/src/a.ts"]);
//! assert_eq!(
//!     cfg.options.effective().module_resolution(),
//!     ts_r_config::ModuleResolution::NodeNext
//! );
//! ```

pub mod cli;
pub mod diagnostic;
pub mod enums;
pub mod exports;
pub mod fs;
pub mod glob;
pub mod jsonc;
pub mod options;
pub mod path;
pub mod show_config;
pub mod tsconfig;
pub mod verify;

pub use diagnostic::{ConfigDiagnostic, Severity};
pub use enums::{
    JsxEmit, ModuleDetection, ModuleKind, ModuleResolution, NewLine, ScriptTarget, lib_file_name,
};
pub use fs::{DirEntry, EntryKind, FileStamp, FileSystem, MemoryFs, RealFs};
pub use options::{CompilerOptions, Effective, OptionId, OptionValue, PathsMap, StrictFlag};
pub use show_config::show_config;
pub use tsconfig::{
    ConfigLoader, ParsedConfig, ProjectReference, SpecList, TypeAcquisition, WatchOptions,
    find_config_file, load_tsconfig, resolve_project_arg,
};
pub use verify::verify_options;
