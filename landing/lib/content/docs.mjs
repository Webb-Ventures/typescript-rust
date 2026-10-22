// Docs pages, error-code pages, the search index, llms.txt and the Markdown copies, read from
// content/ (Markdown with front matter) and data/. Plain JavaScript so both the Next.js pages and
// the Node scripts (scripts/check.mjs, scripts/verify-examples.mjs) can use it.
//
//   content/index.md, getting-started.md, sound-mode.md, status.md   -> /docs/<name>/
//   content/tools/<tool>.md                                           -> /docs/tools/<tool>/
//   content/errors/<CODE>.md                                          -> /errors/<CODE>/
//
// Links between sources are relative .md links (so they work on GitHub); root links such as
// /benchmarks/#hmr are site URLs (checked against the export by scripts/check.mjs); links to
// other repo files become GitHub links. Every problem found is collected in `problems`: the
// pages refuse to build while there are any. See AGENTS.md.

import fs from 'node:fs';
import path from 'node:path';
import { frontMatter, render, inline, esc } from './markdown.mjs';
import { exampleBlocks, exampleHash, effectiveTsconfig } from './examples.mjs';
import { published } from '../commits.mjs';

export const REPO = 'https://github.com/Webb-Ventures/typescript-rust';

// Run from landing/ (npm scripts and `next build` do).
export const LANDING = process.cwd();
export const ROOT = path.dirname(LANDING);
const CONTENT = path.join(LANDING, 'content');
const DATA = path.join(LANDING, 'data');

/** The site's base URL with a trailing slash (SITE_URL, as lib/site.ts). */
export const BASE = `${(process.env.SITE_URL || 'https://ts-r.dev').replace(/\/$/, '')}/`;

// Docs navigation sections, in order.
export const SECTIONS = ['Start', 'Tools', 'Reference'];

const CRATE_LABEL = {
  ts_r_diagnostics: 'the lexer and parser',
  ts_r_binder: 'the binder',
  ts_r_checker: 'the type checker',
  ts_r_config: 'the tsconfig loader',
  ts_r_check: 'the ts-r-check command line',
};

// Ids the page shell uses; Markdown headings must not take them.
const RESERVED_IDS = ['main', 'title', 'lg', 'lg-foot'];

const readJson = file => JSON.parse(fs.readFileSync(file, 'utf8'));
export const stripMd = s => String(s).replace(/`/g, '');
export const plainMessage = m => m.replace(/\{\d+\}/g, '…').split('\n')[0];
const errOrder = c => (c.startsWith('TSR') ? 1e6 + Number(c.slice(3)) : Number(c.slice(2)));

function walkMd(dir, out = []) {
  if (!fs.existsSync(dir)) return out;
  for (const e of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walkMd(p, out);
    else if (e.name.endsWith('.md')) out.push(p);
  }
  return out;
}

function docPath(rel) {
  if (rel === 'index.md') return 'docs/';
  return `docs/${rel.replace(/\.md$/, '')}/`;
}

let cached = null;

/** Everything the docs, error, search and llms.txt routes need. Built once per process. */
export function content() {
  if (!cached) cached = build();
  return cached;
}

/** Throws when a source has a problem, so a broken page is never published. */
export function assertContent() {
  const c = content();
  if (c.problems.length) throw new Error(`content problems (run npm run check):\n${c.problems.join('\n')}`);
  return c;
}

function build() {
  const problems = [];

  // ------------------------------------------------------------ load sources
  const docs = [];
  for (const file of walkMd(CONTENT)) {
    const rel = path.relative(CONTENT, file).split(path.sep).join('/');
    if (rel.startsWith('errors/')) continue;
    const { data, body } = frontMatter(fs.readFileSync(file, 'utf8'));
    for (const k of ['title', 'description', 'section', 'order']) {
      if (data[k] === undefined) problems.push(`content/${rel}: front matter needs ${k}`);
    }
    if (data.description && data.description.length > 170) problems.push(`content/${rel}: description is ${data.description.length} characters (at most 170)`);
    docs.push({ file, rel, data, body, path: docPath(rel) });
  }
  docs.sort((a, b) => SECTIONS.indexOf(a.data.section) - SECTIONS.indexOf(b.data.section) || a.data.order - b.data.order);

  const diag = new Map(readJson(path.join(DATA, 'diagnostics.json')).codes.map(c => [`TS${c.code}`, c]));
  const examplesPath = path.join(DATA, 'error-examples.json');
  const runs = fs.existsSync(examplesPath) ? readJson(examplesPath) : {};
  const errors = [];
  const errDir = path.join(CONTENT, 'errors');
  for (const name of fs.existsSync(errDir) ? fs.readdirSync(errDir).sort() : []) {
    if (!name.endsWith('.md')) continue;
    const file = path.join(errDir, name);
    const code = name.slice(0, -3);
    const { data, body } = frontMatter(fs.readFileSync(file, 'utf8'));
    const rel = `errors/${name}`;
    if (data.code !== code) problems.push(`content/${rel}: front matter code must be ${code}`);
    if (!data.summary) problems.push(`content/${rel}: front matter needs summary`);
    else if (data.summary.length > 170) problems.push(`content/${rel}: summary is ${data.summary.length} characters (at most 170)`);
    const planned = code.startsWith('TSR');
    const entry = diag.get(code);
    if (!planned && !entry) problems.push(`content/${rel}: ${code} is not in data/diagnostics.json (cargo xtask diag-codes)`);
    if (planned && !data.message) problems.push(`content/${rel}: sound-mode codes need a message in front matter`);
    errors.push({
      file, rel, code, data, body, planned,
      message: planned ? data.message : entry?.message ?? '',
      category: planned ? 'Error' : entry?.category ?? 'Error',
      crates: entry?.crates ?? [],
      path: `errors/${code}/`,
    });
  }
  errors.sort((a, b) => errOrder(a.code) - errOrder(b.code));

  // ------------------------------------------------------------ links
  const pageOfSource = new Map();
  for (const d of docs) pageOfSource.set(d.file, d.path);
  for (const e of errors) pageOfSource.set(e.file, e.path);

  // Resolves a link written in `srcFile`. Page links become root URLs ("/docs/x/"), or full
  // URLs with `absolute` (the Markdown copies). Records broken links as problems.
  const linker = (srcFile, absolute = false) => href => {
    if (/^(https?:|mailto:)/.test(href) || href.startsWith('#')) return href;
    const site = p => (absolute ? BASE + p : `/${p}`);
    if (href.startsWith('/')) return site(href.slice(1));
    const [p, hash = ''] = href.split('#');
    const abs = path.resolve(path.dirname(srcFile), decodeURIComponent(p));
    const frag = hash ? `#${hash}` : '';
    if (pageOfSource.has(abs)) return site(pageOfSource.get(abs)) + frag;
    const repoRel = path.relative(ROOT, abs).split(path.sep).join('/');
    if (!repoRel.startsWith('..') && fs.existsSync(abs)) {
      return `${REPO}/${fs.statSync(abs).isDirectory() ? 'tree' : 'blob'}/main/${repoRel}${frag}`;
    }
    problems.push(`${path.relative(ROOT, srcFile)}: broken link ${href}`);
    return href;
  };

  const navItems = docs.map(d => ({ path: d.path, label: d.data.nav ?? stripMd(d.data.title), section: d.data.section }));
  const nav = [
    ...navItems,
    { path: 'errors/', label: 'Error codes', section: 'Reference' },
    { path: 'benchmarks/', label: 'Benchmarks', section: 'Reference' },
  ];

  const searchIndex = [];
  const mdCopies = [];
  const status = statusMarkdown(problems);
  benchProblems(problems);

  // ------------------------------------------------------------ docs pages
  const docPages = docs.map((d, i) => {
    let body = d.body;
    if (d.data.generate === 'status') body = status(body, d.file);
    const { html, headings } = render(body, { link: linker(d.file), reservedIds: RESERVED_IDS });
    const badges = (d.data.badges ?? []).map(b => {
      const [cls, ...text] = String(b).split(':');
      return { cls, text: text.join(':') };
    });
    mdCopies.push([d.path, stripMd(d.data.title), d.data.description, body, d.file]);
    searchIndex.push({ t: stripMd(d.data.title), u: d.path, s: d.data.section === 'Tools' ? 'Tools' : 'Docs', d: d.data.description });
    for (const h of headings.filter(h => h.level === 2)) {
      searchIndex.push({ t: `${stripMd(d.data.title)} › ${h.text}`, u: `${d.path}#${h.id}`, s: 'Section', d: '' });
    }
    return {
      path: d.path,
      rel: d.rel,
      slug: d.rel.replace(/\.md$/, ''),
      title: d.data.title,
      titleHtml: inline(d.data.title),
      label: navItems[i].label,
      description: d.data.description,
      section: d.data.section,
      badges,
      html,
      toc: toc(headings),
      prev: i > 0 ? navItems[i - 1] : null,
      next: i + 1 < navItems.length ? navItems[i + 1] : null,
    };
  });

  // ------------------------------------------------------------ error pages
  let verified = 0;
  const errorPages = errors.map((e, i) => {
    const ex = exampleBlocks(e.body);
    const run = runs[e.code];
    let body = e.body;
    let exampleOk = false;
    if (ex.files.length) {
      if (!run || run.hash !== exampleHash(e.code, ex)) {
        problems.push(`content/${e.rel}: example not verified (run node scripts/verify-examples.mjs ${e.code})`);
      } else if (run.triggered) {
        exampleOk = true;
        verified++;
      }
      if (!exampleOk) body = dropSection(body, 'Example');
    }
    const shown = new Set();
    const code = (info, text) => {
      const words = info.split(/\s+/);
      const lang = words[0];
      if (words.includes('example')) {
        const f = ex.files.find(x => x.text === text && !shown.has(x));
        shown.add(f);
        return figure(f?.file ?? `index.${lang}`, text, lang);
      }
      if (words.includes('tsconfig')) return figure('tsconfig.json', effectiveTsconfig(ex).trimEnd(), 'json');
      if (words.includes('spec')) return figure("Spec case from design record 0007 (checked by the record's case harness, not by this site's build)", text, lang, 'spec');
      return null;
    };
    let { html, headings } = render(body, { link: linker(e.file), code, reservedIds: RESERVED_IDS });
    if (exampleOk) {
      const out = `<figure class="term example output">
<figcaption><code>${esc(run.command)}</code> <span class="tag">ts-r output</span></figcaption>
<pre><code>${esc(run.output.trimEnd())}</code></pre>
</figure>
<p class="small muted">Recorded by <code>node scripts/verify-examples.mjs</code> with ts-r built from commit ${published(run.commit) ? `<a href="${REPO}/commit/${esc(run.commit)}"><code>${esc(run.commit)}</code></a>` : `<code>${esc(run.commit)}</code>`}.${ex.tsconfig ? '' : ' The example uses the default tsconfig: <code>strict</code>, <code>target: es2022</code>, <code>module: esnext</code>, <code>moduleResolution: bundler</code>.'}</p>`;
      html = insertAfterSection(html, 'example', out);
    }
    const badges = e.planned
      ? [{ cls: 'warn', text: `Sound mode ${e.data.rule ?? ''}, pre-alpha` }, { cls: '', text: 'Design record 0007, Accepted' }]
      : [{ cls: '', text: e.category }, ...e.crates.map(c => CRATE_LABEL[c]).filter(Boolean).map(l => ({ cls: '', text: `Reported by ${l}` }))];
    const plain = plainMessage(e.message);
    mdCopies.push([e.path, `${e.code}: ${plain}`, e.data.summary, body, e.file]);
    searchIndex.push({ t: e.code, u: e.path, s: e.planned ? 'Sound-mode error' : 'Error', d: plain });
    return {
      code: e.code,
      path: e.path,
      planned: e.planned,
      message: e.message,
      messageHtml: esc(e.message).replace(/\{(\d+)\}/g, '<var>{$1}</var>'),
      plain,
      summary: e.data.summary,
      rule: e.planned && e.data.rule_title ? { rule: e.data.rule, title: e.data.rule_title, level: e.data.level } : null,
      badges,
      html,
      toc: toc(headings),
      prev: i > 0 ? { path: errors[i - 1].path, label: errors[i - 1].code } : null,
      next: i + 1 < errors.length ? { path: errors[i + 1].path, label: errors[i + 1].code } : null,
    };
  });

  searchIndex.push({ t: 'Error codes', u: 'errors/', s: 'Docs', d: 'Every error code page: tsc codes ts-r reports, TS8xxx, ts-r-only codes and sound-mode codes.' });
  searchIndex.push({ t: 'Benchmarks', u: 'benchmarks/', s: 'Docs', d: 'Measured numbers for every tool and how each was measured.' });

  // ------------------------------------------------------------ Markdown copies and llms.txt
  const markdown = new Map();
  for (const [p, title, desc, body, file] of mdCopies) {
    markdown.set(p, `# ${title}\n\n> ${desc}\n\n${absoluteLinks(stripFenceInfo(body), linker(file, true)).trim()}\n\nSource: ${BASE}${p}\n`);
  }

  return {
    problems,
    nav,
    docs: docPages,
    errors: errorPages,
    errorStats: {
      catalogued: diag.size,
      verified,
      selection: readJson(path.join(DATA, 'error-selection.json')).method,
    },
    searchIndex,
    markdown,
    llms: llms(docPages, errorPages),
  };
}

function toc(headings) {
  return headings.filter(h => h.level === 2 || h.level === 3).map(h => ({ level: h.level, id: h.id, html: h.html.replace(/<a [^>]*>|<\/a>/g, '') }));
}

function figure(caption, text, lang, cls = '') {
  return `<figure class="term example${cls ? ` ${cls}` : ''}">
<figcaption>${esc(caption)}</figcaption>
<pre data-lang="${esc(lang)}"><code>${esc(text)}</code></pre>
</figure>`;
}

function insertAfterSection(html, id, extra) {
  const start = html.indexOf(`<h2 id="${id}"`);
  if (start < 0) return html + extra;
  const next = html.indexOf('<h2 ', start + 1);
  return next < 0 ? html + '\n' + extra : html.slice(0, next) + extra + '\n' + html.slice(next);
}

function dropSection(md, title) {
  const out = [];
  let skipping = false;
  let fence = false;
  for (const l of md.split('\n')) {
    if (/^(`{3,}|~{3,})/.test(l.trim())) fence = !fence;
    if (!fence && /^##\s/.test(l) && !/^###/.test(l)) skipping = l.replace(/^##\s+/, '').trim() === title;
    if (!skipping) out.push(l);
  }
  return out.join('\n');
}

// Rewrites [text](href) outside code to absolute URLs (for the Markdown copies).
function absoluteLinks(md, link) {
  let fence = false;
  return md.split('\n').map(l => {
    if (/^\s*(`{3,}|~{3,})/.test(l)) { fence = !fence; return l; }
    if (fence) return l;
    return l.replace(/(`[^`]*`)|\]\(([^)\s]+)\)/g, (m, code, href) => (code ? code : `](${link(href)})`));
  }).join('\n');
}

const stripFenceInfo = md => md.replace(/^(\s*`{3,})(\w+)[^\n]*$/gm, '$1$2');

function llms(docs, errors) {
  return `# ts-r

> Fast TypeScript tooling written from scratch in Rust: ts-r-dev, ts-r-build, ts-r-check, ts-r-lint, ts-r-run and ts-r-lsp, plus a tiny ts-r launcher. Each tool is a separate binary that works on its own next to tsc, ESLint and Vite. Alpha: install from npm with \`npm install --save-dev @ts-r.dev/cli@next\`.

Every page below has a Markdown version at the same address with \`index.md\` appended.

## Docs

${docs.map(d => `- [${stripMd(d.title)}](${BASE}${d.path}index.md): ${d.description}`).join('\n')}
- [Benchmarks](${BASE}benchmarks/): measured numbers for every tool, each with its method and source commit. Raw data: ${BASE}data/bench.json

## Error codes

${errors.map(e => `- [${e.code}](${BASE}${e.path}index.md): ${e.plain}${e.planned ? ' (sound mode)' : ''}`).join('\n')}
`;
}

// ---------------------------------------------------------------- status page data

// Replaces <!-- generated:ecosystem --> and <!-- generated:conformance --> in content/status.md.
function statusMarkdown(problems) {
  return (body, file) => {
    for (const m of ['ecosystem', 'conformance']) {
      if (!body.includes(`<!-- generated:${m} -->`)) problems.push(`content/status.md: missing <!-- generated:${m} -->`);
    }
    return body
      .replace('<!-- generated:ecosystem -->', ecosystemMarkdown(problems))
      .replace('<!-- generated:conformance -->', conformanceMarkdown(problems, file));
  };
}

// The stage table, bundle size line and top causes from ecosystem/STATUS.md.
function ecosystemMarkdown(problems) {
  const text = fs.readFileSync(path.join(ROOT, 'ecosystem/STATUS.md'), 'utf8');
  const lines = text.split('\n');
  const tableAfter = (pred, maxRows = Infinity) => {
    const i = lines.findIndex(pred);
    if (i < 0) return [];
    let j = i;
    while (j < lines.length && !lines[j].startsWith('|')) j++;
    const out = [];
    while (j < lines.length && lines[j].startsWith('|') && out.length < maxRows + 2) out.push(lines[j++]);
    return out;
  };
  const gen = /^Generated by `cargo xtask ecosystem` on (\S+) at `(\w+)`: the top (\d+) packages[^,]*,\s*(\d+) s/m.exec(text);
  const tools = /^Tools: (.*)$/m.exec(text);
  if (!gen || !tools) { problems.push('ecosystem/STATUS.md: unexpected header; update ecosystemMarkdown() in landing/lib/content/docs.mjs'); return ''; }
  const stages = tableAfter(l => l.startsWith('| Stage | Pass |'));
  const items = tableAfter(l => l.startsWith('| Stage | Items compared |'));
  const size = lines.find(l => /^\d+ packages with both bundles/.test(l)) ?? '';
  const causes = tableAfter(l => l.startsWith('## Top failure causes'), 10).map(row => {
    // Keep #, Stage, Cause, Packages, Items, Owner.
    const cells = row.split('|').slice(1, -1);
    return `|${cells.slice(0, 6).join('|')}|`;
  });
  if (!stages.length || !causes.length) problems.push('ecosystem/STATUS.md: tables not found; update ecosystemMarkdown() in landing/lib/content/docs.mjs');
  return `Run on ${gen[1]} at commit ${published(gen[2]) ? `[\`${gen[2]}\`](${REPO}/commit/${gen[2]})` : `\`${gen[2]}\``} over the top ${gen[3]} npm packages (${gen[4]} s). Reference tools: ${tools[1]}. Pass rate = pass / (pass + fail + error); skips have nothing to compare.

${stages.join('\n')}

${items.join('\n')}

**Bundle size versus esbuild (minified):** ${size}

Top 10 failure causes (all ${'`'}extra${'`'} causes are diagnostics ts-r reports and tsc doesn't):

${causes.join('\n')}

The full list of failing packages and a repro for each top cause are in [ecosystem/STATUS.md](../../ecosystem/STATUS.md).`;
}

// The conformance table from data/conformance.json. Every row quotes its source file; the build
// fails when the quote is no longer there, so a stale number can't stay published.
function conformanceMarkdown(problems, file) {
  const rows = readJson(path.join(DATA, 'conformance.json')).rows;
  const rel = p => path.relative(path.dirname(file), path.join(ROOT, p)).split(path.sep).join('/');
  const out = ['| Area | Compared against | Result | Source |', '| --- | --- | --- | --- |'];
  for (const r of rows) {
    let cite = '';
    if (r.source) {
      const src = path.join(ROOT, r.source);
      if (!fs.existsSync(src)) { problems.push(`data/conformance.json: ${r.source} does not exist`); continue; }
      const flat = fs.readFileSync(src, 'utf8').replace(/\s+/g, ' ');
      if (!r.quote || !flat.includes(r.quote)) problems.push(`data/conformance.json: "${r.quote}" is not in ${r.source}; update the row`);
      cite = `[${r.source.split('/').slice(-2).join('/')}](${rel(r.source)})`;
    }
    if (r.commit) cite += `${cite ? ', ' : ''}commit ${published(r.commit) ? `[\`${r.commit}\`](${REPO}/commit/${r.commit})` : `\`${r.commit}\``}`;
    if (!cite) problems.push(`data/conformance.json: row "${r.area}" needs a source or a commit`);
    out.push(`| ${r.area} | ${r.against} | ${r.result} | ${cite} |`);
  }
  return out.join('\n');
}

// ---------------------------------------------------------------- benchmark data

const KINDS = ['ours', 'other', 'floor'];

// data/bench.json rules (AGENTS.md, "The rule for numbers"): every chart says how it was measured.
function benchProblems(problems) {
  const data = readJson(path.join(DATA, 'bench.json'));
  for (const s of data.sections) {
    if (!s.charts && !s.table && s.tableFrom !== 'conformance') problems.push(`data/bench.json section ${s.id}: needs charts, a table or "tableFrom": "conformance"`);
    if (s.charts && !s.method) problems.push(`data/bench.json section ${s.id}: every chart needs a method (how it was measured)`);
    if (s.method) {
      for (const k of ['measured', 'machine', 'software', 'input', 'command', 'commit', 'sources']) {
        if (!s.method[k]) problems.push(`data/bench.json section ${s.id}: method.${k} is missing`);
      }
      if (s.method.machine && !data.machines[s.method.machine]) problems.push(`data/bench.json section ${s.id}: machine "${s.method.machine}" is not in machines`);
    }
    for (const c of s.charts ?? []) {
      if (!c.id) problems.push(`data/bench.json section ${s.id}: chart "${c.title}" needs an id`);
      for (const r of c.rows) {
        if (r.label.length > 34) problems.push(`data/bench.json ${c.id}: label "${r.label}" is longer than 34 characters; it won't fit on phones`);
        if (!KINDS.includes(r.kind)) problems.push(`data/bench.json ${c.id}: row kind must be ours, other or floor`);
      }
    }
  }
}
