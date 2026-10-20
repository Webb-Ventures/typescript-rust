//! The tools the launcher knows about.

/// One `ts-r-<name>` binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tool {
    /// Subcommand name (`ts-r <name>`).
    pub name: &'static str,
    /// Roadmap milestone the tool ships in.
    pub milestone: &'static str,
    /// One-line description for `--help`.
    pub about: &'static str,
}

impl Tool {
    /// Binary file name, e.g. `ts-r-run` (`ts-r-run.exe` on Windows).
    pub fn binary_name(&self) -> String {
        format!("ts-r-{}{}", self.name, std::env::consts::EXE_SUFFIX)
    }

    /// Binary name without the platform suffix, for messages.
    pub fn display_name(&self) -> String {
        format!("ts-r-{}", self.name)
    }
}

/// Every tool, in roadmap order.
pub const TOOLS: &[Tool] = &[
    Tool {
        name: "run",
        milestone: "M1",
        about: "Run a TypeScript file (replaces tsx, ts-node)",
    },
    Tool {
        name: "dev",
        milestone: "M2",
        about: "Dev server with HMR (replaces vite dev)",
    },
    Tool {
        name: "build",
        milestone: "M3",
        about: "Production bundler (replaces esbuild, vite build)",
    },
    Tool {
        name: "lint",
        milestone: "M4",
        about: "Linter (replaces ESLint, oxlint)",
    },
    Tool {
        name: "check",
        milestone: "M5",
        about: "Type checker (replaces tsc)",
    },
    Tool {
        name: "lsp",
        milestone: "L1",
        about: "Language server for editors",
    },
];

/// Looks a tool up by subcommand name.
pub fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.name == name)
}

/// The tools `ts-r ci` runs, in output order.
pub const CI_TOOLS: [&str; 3] = ["check", "lint", "build"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup() {
        assert_eq!(find("run").unwrap().milestone, "M1");
        assert!(find("ci").is_none());
        assert!(CI_TOOLS.iter().all(|n| find(n).is_some()));
        assert!(find("lsp").unwrap().binary_name().starts_with("ts-r-lsp"));
    }
}
