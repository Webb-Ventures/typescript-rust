//! A tiny argument parser: commands pull the options they know, then the positionals,
//! then call [`Args::finish`] so unknown flags are reported instead of ignored.

use crate::util::Result;

#[derive(Debug)]
pub struct Args {
    items: Vec<String>,
}

impl Args {
    pub fn from_env() -> Self {
        Self::new(std::env::args().skip(1).collect())
    }

    pub fn new(items: Vec<String>) -> Self {
        Self { items }
    }

    /// Removes `--name` and returns whether it was present.
    pub fn flag(&mut self, name: &str) -> bool {
        let long = format!("--{name}");
        let before = self.items.len();
        self.items.retain(|item| *item != long);
        self.items.len() != before
    }

    /// Removes `--name value` or `--name=value` and returns the value.
    pub fn opt(&mut self, name: &str) -> Result<Option<String>> {
        let long = format!("--{name}");
        let prefix = format!("--{name}=");
        let mut i = 0;
        while i < self.items.len() {
            if self.items[i] == long {
                if i + 1 >= self.items.len() {
                    return Err(format!("{long} needs a value").into());
                }
                let value = self.items.remove(i + 1);
                self.items.remove(i);
                return Ok(Some(value));
            }
            if let Some(value) = self.items[i].strip_prefix(&prefix) {
                let value = value.to_string();
                self.items.remove(i);
                return Ok(Some(value));
            }
            i += 1;
        }
        Ok(None)
    }

    /// Removes and returns the next argument that is not a `--flag`.
    pub fn positional(&mut self) -> Option<String> {
        let i = self.items.iter().position(|item| !item.starts_with("--"))?;
        Some(self.items.remove(i))
    }

    /// Removes and returns the first argument if it is one of `names` (a sub-command given
    /// before any option, so option values are never mistaken for it).
    pub fn subcommand(&mut self, names: &[&str]) -> Option<String> {
        if self
            .items
            .first()
            .is_some_and(|f| names.contains(&f.as_str()))
        {
            Some(self.items.remove(0))
        } else {
            None
        }
    }

    /// Removes and returns every remaining argument, flags included, untouched.
    pub fn take_all(&mut self) -> Vec<String> {
        std::mem::take(&mut self.items)
    }

    /// Removes and returns every remaining positional argument.
    pub fn rest(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(p) = self.positional() {
            out.push(p);
        }
        out
    }

    /// Fails if anything was not consumed.
    pub fn finish(self) -> Result<()> {
        if self.items.is_empty() {
            Ok(())
        } else {
            Err(format!("unexpected arguments: {}", self.items.join(" ")).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Args {
        Args::new(s.split_whitespace().map(String::from).collect())
    }

    #[test]
    fn options_flags_and_positionals() {
        let mut a = args("diff --out x.md v1 --json v2 --to=7.0.3");
        assert!(a.flag("json"));
        assert!(!a.flag("json"));
        assert_eq!(a.opt("out").unwrap().as_deref(), Some("x.md"));
        assert_eq!(a.opt("to").unwrap().as_deref(), Some("7.0.3"));
        assert_eq!(a.rest(), vec!["diff", "v1", "v2"]);
        a.finish().unwrap();
        let mut a = args("--only x,y");
        assert_eq!(a.subcommand(&["run"]), None);
        let mut a = args("run --only x");
        assert_eq!(a.subcommand(&["run"]).as_deref(), Some("run"));
    }

    #[test]
    fn missing_value_and_leftovers_are_errors() {
        let mut a = args("--out");
        assert!(a.opt("out").is_err());
        let a = args("--bogus");
        assert!(a.finish().is_err());
    }
}
