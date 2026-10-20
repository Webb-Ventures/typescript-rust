//! Enumerated compiler option values (`target`, `module`, ...).
//!
//! Each enum lists every spelling tsc 7 accepts (including values TS 7 removed but still
//! parses so it can report TS5108), the spelling `--showConfig` prints (the first alias),
//! and the list tsc prints in TS6046 "Argument for '--x' option must be: ...".

macro_rules! option_enum {
    (
        $(#[$meta:meta])*
        $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident ),* $(,)?
        }
        names: [ $( ($text:literal, $v:ident) ),* $(,)? ]
        valid: [ $( $valid:literal ),* $(,)? ]
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )*
        }

        impl $name {
            /// Every accepted spelling (lower case) and its value, first alias first.
            pub const NAMES: &'static [(&'static str, $name)] = &[ $( ($text, $name::$v), )* ];
            /// The spellings tsc lists in TS6046.
            pub const VALID: &'static [&'static str] = &[ $( $valid, )* ];

            /// Parses a spelling case-insensitively.
            pub fn parse(s: &str) -> Option<Self> {
                let lower = s.to_ascii_lowercase();
                Self::NAMES.iter().find(|(n, _)| *n == lower).map(|(_, v)| *v)
            }

            /// The spelling `--showConfig` prints.
            pub fn name(self) -> &'static str {
                Self::NAMES.iter().find(|(_, v)| *v == self).map(|(n, _)| *n).unwrap_or("")
            }
        }
    };
}

option_enum! {
    /// `target`.
    ScriptTarget {
        /// Removed in TS 7 (TS5108) but still parsed.
        ES5,
        ES2015, ES2016, ES2017, ES2018, ES2019, ES2020, ES2021, ES2022, ES2023, ES2024, ES2025,
        ESNext,
    }
    names: [
        ("es5", ES5), ("es6", ES2015), ("es2015", ES2015), ("es2016", ES2016), ("es2017", ES2017),
        ("es2018", ES2018), ("es2019", ES2019), ("es2020", ES2020), ("es2021", ES2021),
        ("es2022", ES2022), ("es2023", ES2023), ("es2024", ES2024), ("es2025", ES2025),
        ("esnext", ESNext),
    ]
    valid: [
        "es6", "es2015", "es2016", "es2017", "es2018", "es2019", "es2020", "es2021", "es2022",
        "es2023", "es2024", "es2025", "esnext",
    ]
}

impl ScriptTarget {
    /// Name used in messages such as "Default library for target 'ES2025'".
    pub fn display_name(self) -> &'static str {
        match self {
            Self::ES5 => "ES5",
            Self::ES2015 => "ES2015",
            Self::ES2016 => "ES2016",
            Self::ES2017 => "ES2017",
            Self::ES2018 => "ES2018",
            Self::ES2019 => "ES2019",
            Self::ES2020 => "ES2020",
            Self::ES2021 => "ES2021",
            Self::ES2022 => "ES2022",
            Self::ES2023 => "ES2023",
            Self::ES2024 => "ES2024",
            Self::ES2025 => "ES2025",
            Self::ESNext => "ESNext",
        }
    }

    /// File name of the default library for this target (`lib.es2025.full.d.ts`).
    pub fn default_lib_file_name(self) -> &'static str {
        match self {
            Self::ES5 => "lib.d.ts",
            Self::ES2015 => "lib.es6.d.ts",
            Self::ES2016 => "lib.es2016.full.d.ts",
            Self::ES2017 => "lib.es2017.full.d.ts",
            Self::ES2018 => "lib.es2018.full.d.ts",
            Self::ES2019 => "lib.es2019.full.d.ts",
            Self::ES2020 => "lib.es2020.full.d.ts",
            Self::ES2021 => "lib.es2021.full.d.ts",
            Self::ES2022 => "lib.es2022.full.d.ts",
            Self::ES2023 => "lib.es2023.full.d.ts",
            Self::ES2024 => "lib.es2024.full.d.ts",
            Self::ES2025 => "lib.es2025.full.d.ts",
            Self::ESNext => "lib.esnext.full.d.ts",
        }
    }
}

option_enum! {
    /// `module`.
    ModuleKind {
        /// `commonjs`
        CommonJS,
        /// Removed in TS 7.
        AMD,
        /// Removed in TS 7.
        UMD,
        /// Removed in TS 7.
        System,
        /// `es2015` / `es6`
        ES2015,
        /// `es2020`
        ES2020,
        /// `es2022`
        ES2022,
        /// `esnext`
        ESNext,
        /// `node16`
        Node16,
        /// `node18`
        Node18,
        /// `node20`
        Node20,
        /// `nodenext`
        NodeNext,
        /// `preserve`
        Preserve,
    }
    names: [
        ("commonjs", CommonJS), ("amd", AMD), ("umd", UMD), ("system", System),
        ("es6", ES2015), ("es2015", ES2015), ("es2020", ES2020), ("es2022", ES2022),
        ("esnext", ESNext), ("node16", Node16), ("node18", Node18), ("node20", Node20),
        ("nodenext", NodeNext), ("preserve", Preserve),
    ]
    valid: [
        "commonjs", "es6", "es2015", "es2020", "es2022", "esnext", "node16", "node18", "node20",
        "nodenext", "preserve",
    ]
}

impl ModuleKind {
    /// `node16`, `node18`, `node20` or `nodenext`.
    pub fn is_node(self) -> bool {
        matches!(
            self,
            Self::Node16 | Self::Node18 | Self::Node20 | Self::NodeNext
        )
    }

    /// Spelling used in TS5108 "Option 'module=AMD' has been removed".
    pub fn removed_display(self) -> Option<&'static str> {
        match self {
            Self::AMD => Some("AMD"),
            Self::UMD => Some("UMD"),
            Self::System => Some("System"),
            _ => None,
        }
    }
}

option_enum! {
    /// `moduleResolution`.
    ModuleResolution {
        /// `node` / `node10`: removed in TS 7, behaves like `bundler`.
        Node10,
        /// `classic`: removed in TS 7, behaves like `bundler`.
        Classic,
        /// `node16`
        Node16,
        /// `nodenext`
        NodeNext,
        /// `bundler`
        Bundler,
    }
    names: [
        ("node", Node10), ("node10", Node10), ("classic", Classic), ("node16", Node16),
        ("nodenext", NodeNext), ("bundler", Bundler),
    ]
    valid: ["node16", "nodenext", "bundler"]
}

impl ModuleResolution {
    /// Name used in trace output ("using 'NodeNext'").
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Node10 => "Node10",
            Self::Classic => "Classic",
            Self::Node16 => "Node16",
            Self::NodeNext => "NodeNext",
            Self::Bundler => "Bundler",
        }
    }
}

option_enum! {
    /// `moduleDetection`.
    ModuleDetection { Auto, Legacy, Force }
    names: [("auto", Auto), ("legacy", Legacy), ("force", Force)]
    valid: ["auto", "legacy", "force"]
}

option_enum! {
    /// `jsx`.
    JsxEmit { Preserve, ReactNative, ReactJsx, ReactJsxDev, React }
    names: [
        ("preserve", Preserve), ("react-native", ReactNative), ("react-jsx", ReactJsx),
        ("react-jsxdev", ReactJsxDev), ("react", React),
    ]
    valid: ["preserve", "react-native", "react-jsx", "react-jsxdev", "react"]
}

option_enum! {
    /// `newLine`.
    NewLine { Crlf, Lf }
    names: [("crlf", Crlf), ("lf", Lf)]
    valid: ["crlf", "lf"]
}

option_enum! {
    /// `watchOptions.watchFile`.
    WatchFileKind {
        FixedPollingInterval, PriorityPollingInterval, DynamicPriorityPolling,
        FixedChunkSizePolling, UseFsEvents, UseFsEventsOnParentDirectory,
    }
    names: [
        ("fixedpollinginterval", FixedPollingInterval),
        ("prioritypollinginterval", PriorityPollingInterval),
        ("dynamicprioritypolling", DynamicPriorityPolling),
        ("fixedchunksizepolling", FixedChunkSizePolling),
        ("usefsevents", UseFsEvents),
        ("usefseventsonparentdirectory", UseFsEventsOnParentDirectory),
    ]
    valid: [
        "fixedpollinginterval", "prioritypollinginterval", "dynamicprioritypolling",
        "fixedchunksizepolling", "usefsevents", "usefseventsonparentdirectory",
    ]
}

option_enum! {
    /// `watchOptions.watchDirectory`.
    WatchDirectoryKind {
        UseFsEvents, FixedPollingInterval, DynamicPriorityPolling, FixedChunkSizePolling,
    }
    names: [
        ("usefsevents", UseFsEvents), ("fixedpollinginterval", FixedPollingInterval),
        ("dynamicprioritypolling", DynamicPriorityPolling),
        ("fixedchunksizepolling", FixedChunkSizePolling),
    ]
    valid: ["usefsevents", "fixedpollinginterval", "dynamicprioritypolling", "fixedchunksizepolling"]
}

option_enum! {
    /// `watchOptions.fallbackPolling`.
    PollingKind { FixedInterval, PriorityInterval, DynamicPriority, FixedChunkSize }
    names: [
        ("fixedinterval", FixedInterval), ("priorityinterval", PriorityInterval),
        ("dynamicpriority", DynamicPriority), ("fixedchunksize", FixedChunkSize),
    ]
    valid: ["fixedinterval", "priorityinterval", "dynamicpriority", "fixedchunksize"]
}

/// `lib` entries in tsc 7's order: (spelling, file name). The first spelling for a file is
/// the one `--showConfig` prints.
pub const LIBS: &[(&str, &str)] = &[
    ("es5", "lib.es5.d.ts"),
    ("es6", "lib.es2015.d.ts"),
    ("es2015", "lib.es2015.d.ts"),
    ("es7", "lib.es2016.d.ts"),
    ("es2016", "lib.es2016.d.ts"),
    ("es2017", "lib.es2017.d.ts"),
    ("es2018", "lib.es2018.d.ts"),
    ("es2019", "lib.es2019.d.ts"),
    ("es2020", "lib.es2020.d.ts"),
    ("es2021", "lib.es2021.d.ts"),
    ("es2022", "lib.es2022.d.ts"),
    ("es2023", "lib.es2023.d.ts"),
    ("es2024", "lib.es2024.d.ts"),
    ("es2025", "lib.es2025.d.ts"),
    ("esnext", "lib.esnext.d.ts"),
    ("dom", "lib.dom.d.ts"),
    ("dom.iterable", "lib.dom.iterable.d.ts"),
    ("dom.asynciterable", "lib.dom.asynciterable.d.ts"),
    ("webworker", "lib.webworker.d.ts"),
    (
        "webworker.importscripts",
        "lib.webworker.importscripts.d.ts",
    ),
    ("webworker.iterable", "lib.webworker.iterable.d.ts"),
    (
        "webworker.asynciterable",
        "lib.webworker.asynciterable.d.ts",
    ),
    ("scripthost", "lib.scripthost.d.ts"),
    ("es2015.core", "lib.es2015.core.d.ts"),
    ("es2015.collection", "lib.es2015.collection.d.ts"),
    ("es2015.generator", "lib.es2015.generator.d.ts"),
    ("es2015.iterable", "lib.es2015.iterable.d.ts"),
    ("es2015.promise", "lib.es2015.promise.d.ts"),
    ("es2015.proxy", "lib.es2015.proxy.d.ts"),
    ("es2015.reflect", "lib.es2015.reflect.d.ts"),
    ("es2015.symbol", "lib.es2015.symbol.d.ts"),
    (
        "es2015.symbol.wellknown",
        "lib.es2015.symbol.wellknown.d.ts",
    ),
    ("es2016.array.include", "lib.es2016.array.include.d.ts"),
    ("es2016.intl", "lib.es2016.intl.d.ts"),
    ("es2017.arraybuffer", "lib.es2017.arraybuffer.d.ts"),
    ("es2017.date", "lib.es2017.date.d.ts"),
    ("es2017.object", "lib.es2017.object.d.ts"),
    ("es2017.sharedmemory", "lib.es2017.sharedmemory.d.ts"),
    ("es2017.string", "lib.es2017.string.d.ts"),
    ("es2017.intl", "lib.es2017.intl.d.ts"),
    ("es2017.typedarrays", "lib.es2017.typedarrays.d.ts"),
    ("es2018.asyncgenerator", "lib.es2018.asyncgenerator.d.ts"),
    ("es2018.asynciterable", "lib.es2018.asynciterable.d.ts"),
    ("es2018.intl", "lib.es2018.intl.d.ts"),
    ("es2018.promise", "lib.es2018.promise.d.ts"),
    ("es2018.regexp", "lib.es2018.regexp.d.ts"),
    ("es2019.array", "lib.es2019.array.d.ts"),
    ("es2019.object", "lib.es2019.object.d.ts"),
    ("es2019.string", "lib.es2019.string.d.ts"),
    ("es2019.symbol", "lib.es2019.symbol.d.ts"),
    ("es2019.intl", "lib.es2019.intl.d.ts"),
    ("es2020.bigint", "lib.es2020.bigint.d.ts"),
    ("es2020.date", "lib.es2020.date.d.ts"),
    ("es2020.promise", "lib.es2020.promise.d.ts"),
    ("es2020.sharedmemory", "lib.es2020.sharedmemory.d.ts"),
    ("es2020.string", "lib.es2020.string.d.ts"),
    (
        "es2020.symbol.wellknown",
        "lib.es2020.symbol.wellknown.d.ts",
    ),
    ("es2020.intl", "lib.es2020.intl.d.ts"),
    ("es2020.number", "lib.es2020.number.d.ts"),
    ("es2021.promise", "lib.es2021.promise.d.ts"),
    ("es2021.string", "lib.es2021.string.d.ts"),
    ("es2021.weakref", "lib.es2021.weakref.d.ts"),
    ("es2021.intl", "lib.es2021.intl.d.ts"),
    ("es2022.array", "lib.es2022.array.d.ts"),
    ("es2022.error", "lib.es2022.error.d.ts"),
    ("es2022.intl", "lib.es2022.intl.d.ts"),
    ("es2022.object", "lib.es2022.object.d.ts"),
    ("es2022.string", "lib.es2022.string.d.ts"),
    ("es2022.regexp", "lib.es2022.regexp.d.ts"),
    ("es2023.array", "lib.es2023.array.d.ts"),
    ("es2023.collection", "lib.es2023.collection.d.ts"),
    ("es2023.intl", "lib.es2023.intl.d.ts"),
    ("es2024.arraybuffer", "lib.es2024.arraybuffer.d.ts"),
    ("es2024.collection", "lib.es2024.collection.d.ts"),
    ("es2024.object", "lib.es2024.object.d.ts"),
    ("es2024.promise", "lib.es2024.promise.d.ts"),
    ("es2024.regexp", "lib.es2024.regexp.d.ts"),
    ("es2024.sharedmemory", "lib.es2024.sharedmemory.d.ts"),
    ("es2024.string", "lib.es2024.string.d.ts"),
    ("es2025.collection", "lib.es2025.collection.d.ts"),
    ("es2025.float16", "lib.es2025.float16.d.ts"),
    ("es2025.intl", "lib.es2025.intl.d.ts"),
    ("es2025.iterator", "lib.es2025.iterator.d.ts"),
    ("es2025.promise", "lib.es2025.promise.d.ts"),
    ("es2025.regexp", "lib.es2025.regexp.d.ts"),
    ("esnext.asynciterable", "lib.es2018.asynciterable.d.ts"),
    ("esnext.symbol", "lib.es2019.symbol.d.ts"),
    ("esnext.bigint", "lib.es2020.bigint.d.ts"),
    ("esnext.weakref", "lib.es2021.weakref.d.ts"),
    ("esnext.object", "lib.es2024.object.d.ts"),
    ("esnext.regexp", "lib.es2024.regexp.d.ts"),
    ("esnext.string", "lib.es2024.string.d.ts"),
    ("esnext.float16", "lib.es2025.float16.d.ts"),
    ("esnext.iterator", "lib.es2025.iterator.d.ts"),
    ("esnext.promise", "lib.es2025.promise.d.ts"),
    ("esnext.array", "lib.esnext.array.d.ts"),
    ("esnext.collection", "lib.esnext.collection.d.ts"),
    ("esnext.date", "lib.esnext.date.d.ts"),
    ("esnext.decorators", "lib.esnext.decorators.d.ts"),
    ("esnext.disposable", "lib.esnext.disposable.d.ts"),
    ("esnext.error", "lib.esnext.error.d.ts"),
    ("esnext.intl", "lib.esnext.intl.d.ts"),
    ("esnext.sharedmemory", "lib.esnext.sharedmemory.d.ts"),
    ("esnext.temporal", "lib.esnext.temporal.d.ts"),
    ("esnext.typedarrays", "lib.esnext.typedarrays.d.ts"),
    ("decorators", "lib.decorators.d.ts"),
    ("decorators.legacy", "lib.decorators.legacy.d.ts"),
];

/// File name for a `lib` spelling (case-insensitive), e.g. `DOM` → `lib.dom.d.ts`.
pub fn lib_file_name(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    LIBS.iter().find(|(n, _)| *n == lower).map(|(_, f)| *f)
}

/// The canonical spelling (first alias) for a `lib` entry, as `--showConfig` prints it.
pub fn lib_canonical_name(name: &str) -> Option<&'static str> {
    let file = lib_file_name(name)?;
    LIBS.iter().find(|(_, f)| *f == file).map(|(n, _)| *n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_names() {
        assert_eq!(ScriptTarget::parse("ES2015"), Some(ScriptTarget::ES2015));
        assert_eq!(ScriptTarget::ES2015.name(), "es6");
        assert_eq!(ScriptTarget::parse("es3"), None);
        assert_eq!(ModuleKind::parse("NodeNext"), Some(ModuleKind::NodeNext));
        assert_eq!(
            ModuleResolution::parse("node10").map(|m| m.name()),
            Some("node")
        );
        assert_eq!(JsxEmit::parse("React-JSX"), Some(JsxEmit::ReactJsx));
    }

    #[test]
    fn libs() {
        assert_eq!(lib_file_name("DOM.Iterable"), Some("lib.dom.iterable.d.ts"));
        assert_eq!(lib_canonical_name("ES2015"), Some("es6"));
        assert_eq!(
            lib_canonical_name("esnext.asynciterable"),
            Some("es2018.asynciterable")
        );
        assert_eq!(lib_canonical_name("nope"), None);
    }
}
