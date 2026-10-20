#!/usr/bin/env node
// Checks the sound-mode cases against tsc, the lower bound (sound mode only adds errors).
//
//   scripts/limit.sh --time-s 300 -- node docs/design/0007-sound-mode/cases/check-with-tsc.mjs
//
// Uses the TypeScript version pinned in upstream/UPSTREAM.json (via npx), or the binary in $TSC.
// Pass 1 (tsconfig.json, strict: true): no diagnostics in any file.
// Pass 2 (tsconfig.sound-flags.json, plus the flags sound mode forces): diagnostics only in
//   s04 reject files, exactly on their @expect lines.
// Also checks the case layout: every reject file has @expect markers, accept files have none,
// codes match their rule directory, and each rule has at least 4 accepts and 4 rejects.
import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const casesDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(casesDir, "..", "..", "..", "..");
const pinned = JSON.parse(readFileSync(join(repoRoot, "upstream", "UPSTREAM.json"), "utf8"))
  .typescript.version;

const tscCmd = process.env.TSC
  ? [process.env.TSC]
  : ["npx", "-y", "-p", `typescript@${pinned}`, "tsc"];

function tsc(args) {
  try {
    return execFileSync(tscCmd[0], [...tscCmd.slice(1), ...args], {
      cwd: casesDir,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch (e) {
    if (typeof e.stdout === "string") return e.stdout; // tsc exits non-zero when it reports
    throw e;
  }
}

function diagnostics(output) {
  const out = [];
  for (const line of output.split("\n")) {
    const m = /^(.+?)\((\d+),(\d+)\): error (TS\d+): (.*)$/.exec(line);
    if (m) out.push({ file: m[1].split("\\").join("/"), line: Number(m[2]), code: m[4], text: m[5] });
  }
  return out;
}

function walk(dir, files = []) {
  for (const name of readdirSync(dir).sort()) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p, files);
    else if (name.endsWith(".ts") && !name.endsWith(".d.ts")) files.push(p);
  }
  return files;
}

function expectations(text) {
  const marks = [];
  text.split("\n").forEach((src, i) => {
    const m = /\/\/ @expect((?: TS[R]?\d+)+)\s*$/.exec(src);
    if (m) marks.push({ line: i + 1, codes: m[1].trim().split(/\s+/) });
  });
  return marks;
}

const problems = [];
const version = tsc(["--version"]).trim();
if (!version.endsWith(pinned)) problems.push(`tsc is "${version}", pinned is ${pinned}`);

// Layout.
const counts = new Map();
const files = walk(casesDir).filter((f) => !relative(casesDir, f).startsWith("_shared"));
const s04Expect = new Map();
for (const f of files) {
  const rel = relative(casesDir, f).split("\\").join("/");
  const [ruleDir, kind] = rel.split("/");
  if (kind !== "accept" && kind !== "reject") {
    problems.push(`${rel}: not under accept/ or reject/`);
    continue;
  }
  const c = counts.get(ruleDir) ?? { accept: 0, reject: 0, expects: 0 };
  c[kind] += 1;
  counts.set(ruleDir, c);
  const marks = expectations(readFileSync(f, "utf8"));
  c.expects += marks.reduce((n, m) => n + m.codes.length, 0);
  if (kind === "accept" && marks.length > 0) problems.push(`${rel}: accept file has @expect`);
  if (kind === "reject" && marks.length === 0) problems.push(`${rel}: reject file has no @expect`);
  const rule = /^s(\d\d)-/.exec(ruleDir)?.[1];
  for (const m of marks) {
    for (const code of m.codes) {
      if (!/^TSR\d{4}$/.test(code)) problems.push(`${rel}:${m.line}: ${code} is not a TSR code`);
      const family = code.slice(3, 5);
      if (rule && family !== rule && !(rule === "14" && family === "15")) {
        problems.push(`${rel}:${m.line}: ${code} does not belong to rule S-${rule}`);
      }
    }
  }
  if (ruleDir.startsWith("s04-") && kind === "reject") s04Expect.set(rel, new Set(marks.map((m) => m.line)));
}
for (const [dir, c] of counts) {
  if (dir !== "general" && (c.accept < 4 || c.reject < 4)) {
    problems.push(`${dir}: needs at least 4 accepts and 4 rejects (has ${c.accept}/${c.reject})`);
  }
}

// Pass 1: strict.
const pass1 = diagnostics(tsc(["-p", "tsconfig.json", "--pretty", "false"]));
for (const d of pass1) problems.push(`pass 1 (strict): ${d.file}(${d.line}): ${d.code}: ${d.text}`);

// Pass 2: the flags sound mode forces.
const pass2 = diagnostics(tsc(["-p", "tsconfig.sound-flags.json", "--pretty", "false"]));
const pass2Lines = new Map();
for (const d of pass2) {
  const expected = s04Expect.get(d.file);
  if (!expected || !expected.has(d.line)) {
    problems.push(`pass 2 (sound flags): unexpected ${d.file}(${d.line}): ${d.code}: ${d.text}`);
  }
  if (!pass2Lines.has(d.file)) pass2Lines.set(d.file, new Set());
  pass2Lines.get(d.file).add(d.line);
}
for (const [file, lines] of s04Expect) {
  for (const line of lines) {
    if (!pass2Lines.get(file)?.has(line)) {
      problems.push(`pass 2 (sound flags): ${file}:${line} has @expect but tsc reports nothing there`);
    }
  }
}

// Report.
let accepts = 0, rejects = 0, expects = 0;
console.log("rule directory                     accept  reject  @expect codes");
for (const [dir, c] of [...counts].sort()) {
  console.log(`${dir.padEnd(34)} ${String(c.accept).padStart(6)}  ${String(c.reject).padStart(6)}  ${String(c.expects).padStart(13)}`);
  accepts += c.accept; rejects += c.reject; expects += c.expects;
}
console.log(`${"total".padEnd(34)} ${String(accepts).padStart(6)}  ${String(rejects).padStart(6)}  ${String(expects).padStart(13)}`);
console.log(`${version}: pass 1 (strict) ${pass1.length} diagnostics; pass 2 (sound flags) ${pass2.length} diagnostics, all on s04 @expect lines`);
if (problems.length > 0) {
  for (const p of problems) console.log(`problem: ${p}`);
  console.log(`${problems.length} problem(s)`);
  process.exit(1);
}
console.log("ok");
