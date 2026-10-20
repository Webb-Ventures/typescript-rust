//! The coverage report (`ts-r-check --sound-report[=json]`): files and lines per level, every
//! `@ts-r-unsafe` hatch, unchecked boundaries and per-rule diagnostic counts.

use std::fmt::Write;

use crate::config::Level;
use crate::{SoundResult, codes};

const RULE_NAMES: [&str; 15] = [
    "general",
    "S-01 no any",
    "S-02 no narrowing casts",
    "S-03 no non-null",
    "S-04 index and optional",
    "S-05 container variance",
    "S-06 method variance",
    "S-07 proved guards",
    "S-08 initialisation",
    "S-09 narrowing invalidation",
    "S-10 floating promises",
    "S-11 trust boundaries",
    "S-12 no suppression",
    "S-13 checked exceptions",
    "S-14 external data",
];

fn level_totals(r: &SoundResult) -> [(usize, u64); 4] {
    let mut t = [(0usize, 0u64); 4];
    for f in &r.files {
        let i = match f.level {
            Level::Off => 0,
            Level::Strict => 1,
            Level::Sound => 2,
            Level::Contracts => 3,
        };
        t[i].0 += 1;
        t[i].1 += u64::from(f.lines);
    }
    t
}

/// Plain-text report. Paths are shown through `show`.
pub fn text(r: &SoundResult, show: &dyn Fn(&str) -> String) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "Sound-mode coverage");
    for (i, l) in ["off", "strict", "sound", "contracts"].iter().enumerate() {
        let (files, lines) = level_totals(r)[i];
        let _ = writeln!(s, "  {l:<10} {files:>6} files {lines:>8} lines");
    }
    if !r.rules_off.is_empty() {
        let ids: Vec<String> = r.rules_off.iter().map(|x| format!("S-{x:02}")).collect();
        let _ = writeln!(s, "  partial guarantee: rules off: {}", ids.join(", "));
    }
    let _ = writeln!(s, "Diagnostics per rule");
    for (i, n) in r.per_rule().iter().enumerate() {
        if *n > 0 {
            let _ = writeln!(s, "  {:<30} {n:>6}", RULE_NAMES[i]);
        }
    }
    let _ = writeln!(s, "Escape hatches ({})", r.hatches.len());
    for h in &r.hatches {
        let rules = if h.rules.is_empty() {
            "all rules".to_owned()
        } else {
            h.rules.join(" ")
        };
        let _ = writeln!(s, "  {}:{} [{rules}] {}", show(&h.path), h.line, h.reason);
    }
    let _ = writeln!(s, "Unchecked boundaries ({})", r.boundaries.len());
    for b in &r.boundaries {
        let _ = writeln!(s, "  {b}");
    }
    s
}

fn json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// JSON report (deterministic key order).
pub fn json(r: &SoundResult, show: &dyn Fn(&str) -> String) -> String {
    let mut s = String::from("{\"files\":[");
    for (i, f) in r.files.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str("{\"path\":");
        json_str(&show(&f.path), &mut s);
        let _ = write!(
            s,
            ",\"level\":\"{}\",\"lines\":{}}}",
            f.level.as_str(),
            f.lines
        );
    }
    s.push_str("],\"hatches\":[");
    for (i, h) in r.hatches.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str("{\"path\":");
        json_str(&show(&h.path), &mut s);
        let _ = write!(s, ",\"line\":{},\"rules\":[", h.line);
        for (j, rule) in h.rules.iter().enumerate() {
            if j > 0 {
                s.push(',');
            }
            json_str(rule, &mut s);
        }
        s.push_str("],\"reason\":");
        json_str(&h.reason, &mut s);
        let _ = write!(s, ",\"used\":{}}}", h.used);
    }
    s.push_str("],\"boundaries\":[");
    for (i, b) in r.boundaries.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        json_str(b, &mut s);
    }
    s.push_str("],\"rulesOff\":[");
    for (i, x) in r.rules_off.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let _ = write!(s, "\"S-{x:02}\"");
    }
    s.push_str("],\"perRule\":{");
    let mut first = true;
    for (i, n) in r.per_rule().iter().enumerate() {
        if *n == 0 {
            continue;
        }
        if !first {
            s.push(',');
        }
        first = false;
        let key = if i == 0 {
            "general".to_owned()
        } else {
            format!("S-{i:02}")
        };
        let _ = write!(s, "\"{key}\":{n}");
    }
    s.push_str("},\"diagnostics\":");
    let _ = write!(
        s,
        "{}}}",
        r.diagnostics
            .iter()
            .filter(|d| codes::sound_number(d.code).is_some())
            .count()
    );
    s
}
