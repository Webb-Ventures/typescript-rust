//! `cargo xtask agents-check`: keeps the instructions agents read honest.
//!
//! Fails when a crate (or xtask) has no `AGENTS.md`, when a relative link in an agent-facing
//! markdown file points at a missing file, when a `cargo xtask <cmd>` reference names a command
//! that does not exist, when a `SKILL.md` has invalid front matter, or when a platform module
//! (`windows.rs`, `unix.rs`, ...) is not gated to its platform (see `platform_files`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::args::Args;
use crate::util::{Result, read_to_string, rel, root, walk_files};

pub fn run(args: Args) -> Result<ExitCode> {
    args.finish()?;
    let root = root();
    let mut problems = Vec::new();

    // 1. Every crate has an AGENTS.md.
    let mut crate_dirs = vec![root.join("xtask")];
    if let Ok(entries) = std::fs::read_dir(root.join("crates")) {
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.join("Cargo.toml").is_file())
            .collect();
        dirs.sort();
        crate_dirs.extend(dirs);
    }
    for dir in &crate_dirs {
        if !dir.join("AGENTS.md").is_file() {
            problems.push(format!("{}: missing AGENTS.md", rel(dir)));
        }
    }

    // 2. Collect agent-facing markdown.
    let mut files = Vec::new();
    for name in ["AGENTS.md", "CLAUDE.md", "README.md"] {
        let p = root.join(name);
        if p.is_file() {
            files.push(p);
        }
    }
    for dir in &crate_dirs {
        for name in ["AGENTS.md", "README.md"] {
            let p = dir.join(name);
            if p.is_file() {
                files.push(p);
            }
        }
    }
    for dir in ["docs", ".agents/skills", "bench", "upstream", "ecosystem"] {
        walk_files(&root.join(dir), "md", &mut files);
    }

    // 3. Skills: every skill directory has a SKILL.md with valid front matter.
    let skills_dir = root.join(".agents/skills");
    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let skill = dir.join("SKILL.md");
            if !skill.is_file() {
                problems.push(format!("{}: skill directory without SKILL.md", rel(&dir)));
                continue;
            }
            let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let text = read_to_string(&skill)?;
            for p in check_front_matter(&text, dir_name) {
                problems.push(format!("{}: {p}", rel(&skill)));
            }
        }
    }

    // 4. Links and xtask command references.
    let mut link_count = 0;
    for file in &files {
        let text = read_to_string(file)?;
        let base = file.parent().unwrap_or(&root);
        let mut targets = extract_links(&text);
        if file.file_name().and_then(|n| n.to_str()) == Some("CLAUDE.md") {
            targets.extend(extract_imports(&text));
        }
        for target in targets {
            link_count += 1;
            if let Some(path) = resolve_link(&root, base, &target)
                && !path.exists()
            {
                problems.push(format!("{}: broken link `{target}`", rel(file)));
            }
        }
        for bad in check_xtask_refs(&text) {
            problems.push(format!("{}: unknown xtask command `{bad}`", rel(file)));
        }
    }

    // 5. Platform modules are gated to their platform (CI tests them only there).
    problems.extend(crate::platform_files::check(&root));

    if problems.is_empty() {
        println!(
            "agents-check: ok ({} crates, {} markdown files, {} links)",
            crate_dirs.len(),
            files.len(),
            link_count
        );
        Ok(ExitCode::SUCCESS)
    } else {
        for p in &problems {
            println!("agents-check: {p}");
        }
        println!("agents-check: {} problem(s)", problems.len());
        Ok(ExitCode::from(1))
    }
}

/// Resolves a link target to a path to check, or `None` for external/anchor links.
fn resolve_link(root: &Path, base: &Path, target: &str) -> Option<PathBuf> {
    let t = target.trim();
    if t.is_empty()
        || t.starts_with('#')
        || t.contains("://")
        || t.starts_with("mailto:")
        || t.starts_with("data:")
    {
        return None;
    }
    let t = t.split(['#', '?']).next().unwrap_or(t);
    if t.is_empty() {
        return None;
    }
    let t = t.replace("%20", " ");
    Some(match t.strip_prefix('/') {
        Some(stripped) => root.join(stripped),
        None => base.join(t),
    })
}

/// Splits markdown into prose with fenced code blocks and inline code removed.
fn prose(text: &str) -> String {
    let mut out = String::new();
    let mut fence: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(f) = fence {
            if trimmed.starts_with(f) {
                fence = None;
            }
            out.push('\n');
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
            out.push('\n');
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            out.push('\n');
            continue;
        }
        // Drop inline code spans.
        let mut in_code = false;
        for ch in line.chars() {
            if ch == '`' {
                in_code = !in_code;
            } else if !in_code {
                out.push(ch);
            }
        }
        out.push('\n');
    }
    out
}

/// Inline `[text](target)` and reference `[id]: target` links, outside code.
pub fn extract_links(text: &str) -> Vec<String> {
    let prose = prose(text);
    let mut out = Vec::new();
    let bytes = prose.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b'(' {
            let start = i + 2;
            let mut depth = 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'\n' {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b')' {
                let inner = prose[start..j].trim();
                let target = match inner.strip_prefix('<') {
                    Some(r) => r.split('>').next().unwrap_or(""),
                    None => inner.split_whitespace().next().unwrap_or(""),
                };
                if !target.is_empty() {
                    out.push(target.to_string());
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    for line in prose.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix('[')
            && let Some((label, target)) = rest.split_once("]:")
            && !label.is_empty()
            && !label.starts_with('^')
            && let Some(target) = target.split_whitespace().next()
        {
            out.push(target.trim_matches(['<', '>']).to_string());
        }
    }
    out
}

/// `@path` import lines (CLAUDE.md style).
fn extract_imports(text: &str) -> Vec<String> {
    prose(text)
        .lines()
        .filter_map(|l| l.trim().strip_prefix('@'))
        .filter(|p| !p.is_empty() && !p.contains(char::is_whitespace))
        .map(String::from)
        .collect()
}

const UPSTREAM_SUBCOMMANDS: &[&str] = &["check", "bump", "sync", "diff"];
const CODEGEN_TARGETS: &[&str] = &["diagnostics", "lint-globals"];
const ECOSYSTEM_SUBCOMMANDS: &[&str] = &["run", "refresh", "report", "repro"];

/// Returns every `cargo xtask ...` reference (prose or code) naming an unknown command.
pub fn check_xtask_refs(text: &str) -> Vec<String> {
    let mut bad = Vec::new();
    let mut rest = text;
    while let Some(pos) = rest.find("cargo xtask") {
        rest = &rest[pos + "cargo xtask".len()..];
        let tail: String = rest
            .chars()
            .take_while(|c| *c != '\n' && *c != '`' && *c != '"' && *c != '\'' && *c != ')')
            .collect();
        let mut words = tail.split_whitespace();
        let Some(cmd) = words.next() else { continue };
        if is_placeholder(cmd) {
            continue;
        }
        if !crate::COMMANDS.contains(&cmd) {
            bad.push(cmd.to_string());
            continue;
        }
        let sub = words
            .next()
            .filter(|w| !w.starts_with('-') && !is_placeholder(w));
        let allowed: &[&str] = match cmd {
            "upstream" => UPSTREAM_SUBCOMMANDS,
            "codegen" => CODEGEN_TARGETS,
            "ecosystem" => ECOSYSTEM_SUBCOMMANDS,
            _ => continue,
        };
        if let Some(sub) = sub {
            let sub = sub.trim_end_matches([',', '.', ';', ':']);
            if !allowed.contains(&sub) {
                bad.push(format!("{cmd} {sub}"));
            }
        }
    }
    bad
}

fn is_placeholder(word: &str) -> bool {
    word.starts_with('<')
        || word.starts_with('[')
        || word.starts_with('…')
        || word.starts_with("...")
        || word.starts_with('-')
        || word.starts_with('|')
}

/// Validates SKILL.md YAML front matter: `name` (matching the directory) and `description`.
pub fn check_front_matter(text: &str, dir_name: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return vec!["SKILL.md must start with `---` front matter".into()];
    }
    let mut name = None;
    let mut description = None;
    let mut closed = false;
    for line in lines {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.starts_with(char::is_whitespace) {
                continue;
            }
            let value = value.trim().trim_matches(['"', '\'']).trim().to_string();
            match key.trim() {
                "name" => name = Some(value),
                "description" => description = Some(value),
                _ => {}
            }
        }
    }
    if !closed {
        problems.push("front matter is not closed with `---`".into());
    }
    match name.as_deref() {
        None | Some("") => problems.push("front matter lacks `name`".into()),
        Some(n) => {
            if !n
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                problems.push(format!("skill name `{n}` must be lowercase-kebab-case"));
            }
            if n != dir_name {
                problems.push(format!(
                    "skill name `{n}` does not match directory `{dir_name}`"
                ));
            }
        }
    }
    if description.as_deref().is_none_or(str::is_empty) {
        problems.push("front matter lacks `description`".into());
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_skip_code_and_externals() {
        let md = "See [a](docs/a.md#x) and [b](https://x.y) and `[c](no.md)`.\n\
                  ```\n[d](no2.md)\n```\n[ref]: ../up.md \"title\"\n![img](<img/a b.png>)\n";
        let links = extract_links(md);
        assert_eq!(
            links,
            vec!["docs/a.md#x", "https://x.y", "img/a b.png", "../up.md"]
        );
        let root = Path::new("/r");
        assert_eq!(resolve_link(root, Path::new("/r/d"), "https://x.y"), None);
        assert_eq!(resolve_link(root, Path::new("/r/d"), "#top"), None);
        assert_eq!(
            resolve_link(root, Path::new("/r/d"), "a.md#x"),
            Some(PathBuf::from("/r/d/a.md"))
        );
        assert_eq!(
            resolve_link(root, Path::new("/r/d"), "/AGENTS.md"),
            Some(PathBuf::from("/r/AGENTS.md"))
        );
    }

    #[test]
    fn xtask_refs() {
        let md = "Run `cargo xtask agents-check` then `cargo xtask upstream sync --minimal`.\n\
                  `cargo xtask <cmd>` is fine. `cargo xtask frobnicate` is not, \
                  nor `cargo xtask upstream pull`. `cargo xtask codegen diagnostics --check`.";
        assert_eq!(check_xtask_refs(md), vec!["frobnicate", "upstream pull"]);
    }

    #[test]
    fn front_matter() {
        let ok = "---\nname: bench-compare\ndescription: \"Run benches\"\n---\n# Body\n";
        assert!(check_front_matter(ok, "bench-compare").is_empty());
        let bad = "---\nname: Bench\n---\n";
        let p = check_front_matter(bad, "bench-compare");
        assert_eq!(p.len(), 3, "{p:?}");
        assert_eq!(check_front_matter("# no front matter", "x").len(), 1);
    }
}
