#!/usr/bin/env node
// The site's checks, run by `npm run build` after `next build` (and alone by `npm run check`).
// Run from landing/. Fails, listing every problem, when:
//
// - a content source is invalid: front matter, an error code missing from data/diagnostics.json,
//   an unverified example, a broken link to a repo file, data/bench.json without a method;
// - a quoted number left its source: a data/conformance.json row whose `quote` is no longer in
//   its `source` file (a crate's AGENTS.md or a design record);
// - lib/tools.ts fell out of step with the docs pages in content/tools/;
// - out/ (the export) has a broken internal link or #anchor, or, in coming-soon mode, contains a
//   launched-only page.
//
//   node scripts/check.mjs            all of the above (out/ only if it exists)
//   node scripts/check.mjs --no-out   sources only

import fs from 'node:fs';
import path from 'node:path';
import { content, BASE, LANDING } from '../lib/content/docs.mjs';
import { frontMatter } from '../lib/content/markdown.mjs';

const problems = [...content().problems];

// ---------------------------------------------------------------- lib/tools.ts against the docs

const { TOOLS } = await import('../lib/tools.ts');
const bench = JSON.parse(fs.readFileSync(path.join(LANDING, 'data/bench.json'), 'utf8'));
const toolDocs = fs.readdirSync(path.join(LANDING, 'content/tools')).filter(f => f.endsWith('.md')).map(f => f.slice(0, -3)).sort();
const names = TOOLS.map(t => t.name).sort();
if (names.join() !== toolDocs.join()) problems.push(`lib/tools.ts names ${names.join(', ')} but content/tools/ has ${toolDocs.join(', ')}`);
// The correctness table is data/conformance.json's rows marked `home` (lib/bench.ts).
const correctness = new Set(JSON.parse(fs.readFileSync(path.join(LANDING, 'data/conformance.json'), 'utf8')).rows.filter(r => r.home).map(r => r.home));
for (const t of TOOLS) {
  const file = path.join(LANDING, 'content/tools', `${t.name}.md`);
  if (!fs.existsSync(file)) continue;
  const { body } = frontMatter(fs.readFileSync(file, 'utf8'));
  // The --help block under "## Command line".
  const help = /^## Command line\n[\s\S]*?^```\w*\n([\s\S]*?)^```/m.exec(body)?.[1];
  if (!help) { problems.push(`content/tools/${t.name}.md: no help block under "## Command line" for lib/tools.ts to quote`); continue; }
  if (t.usage.split('\n')[0] !== help.split('\n')[0]) {
    problems.push(`lib/tools.ts ${t.name}: usage starts "${t.usage.split('\n')[0]}" but the docs page's help starts "${help.split('\n')[0]}"`);
  }
  for (const flag of new Set(t.usage.match(/--[a-zA-Z][\w-]*/g) ?? [])) {
    if (!new RegExp(`${flag}(?![\\w-])`).test(help)) problems.push(`lib/tools.ts ${t.name}: usage shows ${flag}, which the docs page's help doesn't list`);
  }
  for (const id of t.sections) {
    if (!bench.sections.some(s => s.id === id)) problems.push(`lib/tools.ts ${t.name}: data/bench.json has no section "${id}"`);
  }
  for (const what of t.correctness) {
    if (!correctness.has(what)) problems.push(`lib/tools.ts ${t.name}: data/conformance.json has no row with "home": "${what}"`);
  }
}

// ---------------------------------------------------------------- links and anchors in out/

const OUT = path.join(LANDING, 'out');
let linkSummary = 'out/ not built: links not checked';
if (!process.argv.includes('--no-out') && fs.existsSync(path.join(OUT, 'index.html'))) {
  const site = JSON.parse(fs.readFileSync(path.join(LANDING, 'data/site.json'), 'utf8'));
  const mode = process.env.SITE_MODE || site.mode;
  if (mode !== 'launched') {
    for (const p of ['docs', 'errors', 'benchmarks', 'search', 'llms.txt', 'search-index.json', 'data']) {
      if (fs.existsSync(path.join(OUT, p))) problems.push(`out/${p}: published in ${mode} mode (launched-only routes are page.launched.tsx / route.launched.ts)`);
    }
  }
  const r = checkLinks(OUT, BASE);
  problems.push(...r.problems);
  linkSummary = `links: ${r.files} files checked, ${r.problems.length} broken internal links or anchors, ${r.external} external URLs (not fetched)`;
}

function walk(dir, out = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (e.name.startsWith('.')) continue;
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, out);
    else out.push(p);
  }
  return out;
}

// Checks every href/src/action in the HTML, and every URL under the base in the HTML, XML, text
// and Markdown files. External links (other hosts) are counted, not fetched.
function checkLinks(root, base) {
  const origin = new URL(base).origin;
  const found = [];
  const external = new Set();
  const idsCache = new Map();
  const ids = file => {
    if (!idsCache.has(file)) idsCache.set(file, new Set([...fs.readFileSync(file, 'utf8').matchAll(/\sid="([^"]+)"/g)].map(m => m[1])));
    return idsCache.get(file);
  };
  const toFile = urlPath => {
    let f = path.join(root, decodeURIComponent(urlPath));
    if (fs.existsSync(f) && fs.statSync(f).isDirectory()) f = path.join(f, 'index.html');
    return fs.existsSync(f) ? f : null;
  };
  // Next's RSC payloads (index.txt, __next.*.txt) repeat what the HTML says.
  const files = walk(root).filter(f => /\.(html|xml|txt|md)$/.test(f) && !/(^|[\\/])(__next\.[^\\/]*|index)\.txt$/.test(f) && !f.includes(`${path.sep}_next${path.sep}`));
  for (const file of files) {
    const rel = path.relative(root, file).split(path.sep).join('/');
    const pageUrl = new URL(rel.replace(/index\.(html|md)$/, ''), base);
    // JSON-LD "@id" values (https://ts-r.dev/#org) name things; they aren't links. They also
    // appear escaped (\"@id\") in the inline RSC payload.
    const text = fs.readFileSync(file, 'utf8').replace(/\\?"@id\\?":\s*\\?"[^"\\]*\\?"/g, '');
    const refs = new Set();
    if (file.endsWith('.html')) for (const m of text.matchAll(/\s(?:href|src|action)="([^"]*)"/g)) refs.add(m[1]);
    for (const m of text.matchAll(/(https?:\/\/[^"'<>()\s\\]+)/g)) if (m[1].startsWith(base)) refs.add(m[1].replace(/[.,;:]+$/, ''));
    for (const ref of refs) {
      if (/^(mailto|tel|data|javascript):/.test(ref)) continue;
      const u = new URL(ref.replace(/&amp;/g, '&'), pageUrl);
      if (u.origin !== origin) {
        if (u.protocol.startsWith('http')) external.add(u.origin + u.pathname);
        continue;
      }
      const target = toFile(u.pathname);
      if (!target) { found.push(`out/${rel}: broken link ${ref}`); continue; }
      if (u.hash && target.endsWith('.html') && !ids(target).has(decodeURIComponent(u.hash.slice(1)))) {
        found.push(`out/${rel}: missing anchor ${ref}`);
      }
    }
  }
  return { problems: found, external: external.size, files: files.length };
}

// ---------------------------------------------------------------- report

for (const p of problems) console.error(`error: ${p}`);
console.log(`content: ${content().docs.length} docs pages, ${content().errors.length} error pages, ${problems.length} problems`);
console.log(linkSummary);
if (problems.length) process.exit(1);
