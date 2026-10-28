//! Preset tags against the published presets (`tests/oracle/presets.txt`, written by
//! `tests/oracle/gen/gen-presets.mjs`): every implemented ESLint and typescript-eslint rule
//! carries exactly the [`Preset`] tags its upstream preset membership implies. Prints how many
//! rules of each preset (jsdoc and unicorn too) are implemented, and which are missing.

use std::collections::{BTreeMap, BTreeSet};

use ts_r_lint::Registry;
use ts_r_lint::rule::Preset;

fn presets() -> BTreeMap<String, BTreeSet<String>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/oracle/presets.txt");
    let text = std::fs::read_to_string(path).expect("presets.txt");
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let (preset, rule) = line.split_once(' ').expect("`<preset> <rule>` line");
        out.entry(preset.to_owned())
            .or_default()
            .insert(rule.to_owned());
    }
    out
}

/// The tags a rule should carry, from the published lists. A typed preset's tag goes only
/// to the rules its untyped twin doesn't have.
fn expected(p: &BTreeMap<String, BTreeSet<String>>, rule: &str) -> BTreeSet<&'static str> {
    let has = |preset: &str| p.get(preset).is_some_and(|s| s.contains(rule));
    let ts_rule = rule.starts_with("@typescript-eslint/");
    let mut want = BTreeSet::new();
    if has("eslint/recommended") {
        want.insert("EslintRecommended");
    }
    if has("typescript-eslint/recommended") {
        if ts_rule {
            want.insert("TsRecommended");
        } else {
            want.insert("TsEslintRecommended");
        }
    }
    if ts_rule && has("typescript-eslint/strict") {
        want.insert("TsStrict");
    }
    if has("typescript-eslint/recommended-type-checked") && !has("typescript-eslint/recommended") {
        want.insert("TsRecommendedTypeChecked");
    }
    if has("typescript-eslint/strict-type-checked") && !has("typescript-eslint/strict") {
        want.insert("TsStrictTypeChecked");
    }
    if ts_rule && has("typescript-eslint/stylistic") {
        want.insert("TsStylistic");
    }
    if has("typescript-eslint/stylistic-type-checked") && !has("typescript-eslint/stylistic") {
        want.insert("TsStylisticTypeChecked");
    }
    want
}

fn tag(p: Preset) -> &'static str {
    match p {
        Preset::EslintRecommended => "EslintRecommended",
        Preset::TsRecommended => "TsRecommended",
        Preset::TsStrict => "TsStrict",
        Preset::TsEslintRecommended => "TsEslintRecommended",
        Preset::TsRecommendedTypeChecked => "TsRecommendedTypeChecked",
        Preset::TsStrictTypeChecked => "TsStrictTypeChecked",
        Preset::TsStylistic => "TsStylistic",
        Preset::TsStylisticTypeChecked => "TsStylisticTypeChecked",
    }
}

#[test]
fn preset_tags_match_upstream() {
    let p = presets();
    let reg = Registry::global();
    let mut wrong = Vec::new();
    for d in reg.all() {
        let name = d.meta.name;
        if name.starts_with("jsdoc/") || name.starts_with("unicorn/") {
            continue;
        }
        let have: BTreeSet<&str> = d.meta.presets.iter().map(|&t| tag(t)).collect();
        let want = expected(&p, name);
        if have != want {
            wrong.push(format!("{name}: has {have:?}, upstream {want:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "preset tags differ:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn preset_coverage() {
    let p = presets();
    let reg = Registry::global();
    for (preset, rules) in &p {
        let missing: Vec<&str> = rules
            .iter()
            .filter(|r| reg.find(r).is_none())
            .map(String::as_str)
            .collect();
        eprintln!(
            "{preset}: {} of {} rules{}{}",
            rules.len() - missing.len(),
            rules.len(),
            if missing.is_empty() {
                ""
            } else {
                "; missing: "
            },
            missing.join(" ")
        );
    }
    // ESLint's and typescript-eslint's presets are complete; keep them so.
    for preset in p
        .keys()
        .filter(|k| k.starts_with("eslint/") || k.starts_with("typescript-eslint/"))
    {
        for r in &p[preset] {
            assert!(
                reg.find(r).is_some(),
                "{preset} rule {r} is not implemented"
            );
        }
    }
}
