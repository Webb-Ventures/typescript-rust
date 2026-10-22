// A small Markdown renderer for the docs and error pages. Written for this repo; no dependencies.
//
// Supported: front matter (`---` block of `key: value` lines), ATX headings (`## Title`,
// optional `{#id}`), paragraphs, `-`/`*`/`1.` lists with nesting by indentation, fenced code
// blocks (``` with an info string), GFM pipe tables, `>` blockquotes, `---` rules, raw HTML
// blocks (a line starting with `<tag`), and inline `code`, **bold**, *italic*, [links](url),
// <https://autolinks> and backslash escapes. Anything else is plain text, escaped.

export const esc = s => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

// ---------------------------------------------------------------- front matter

// `---\nkey: value\n---\n` at the top. Values: "quoted" or 'quoted' strings, numbers,
// true/false, [a, b] lists, or bare text. Returns { data, body }.
export function frontMatter(text) {
  const m = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/.exec(text);
  if (!m) return { data: {}, body: text };
  const data = {};
  for (const line of m[1].split(/\r?\n/)) {
    if (!line.trim() || line.trim().startsWith('#')) continue;
    const i = line.indexOf(':');
    if (i < 0) throw new Error(`front matter: expected "key: value", got "${line}"`);
    data[line.slice(0, i).trim()] = scalar(line.slice(i + 1).trim());
  }
  return { data, body: text.slice(m[0].length) };
}

function scalar(v) {
  if (/^".*"$/.test(v)) return JSON.parse(v);
  if (/^'.*'$/.test(v)) return v.slice(1, -1).replace(/''/g, "'");
  if (v === 'true' || v === 'false') return v === 'true';
  if (/^-?\d+(\.\d+)?$/.test(v)) return Number(v);
  if (/^\[.*\]$/.test(v)) return v.slice(1, -1).split(',').map(s => s.trim()).filter(Boolean).map(scalar);
  return v;
}

// ---------------------------------------------------------------- slugs

export function slugify(text) {
  return text.toLowerCase()
    .replace(/<[^>]+>/g, '')
    .replace(/&[a-z]+;/g, '')
    .replace(/[`*_~]/g, '')
    .replace(/[^\p{L}\p{N}\s-]/gu, '')
    .trim()
    .replace(/\s+/g, '-')
    .replace(/-+/g, '-') || 'section';
}

// ---------------------------------------------------------------- inline

// opts.link(href) rewrites link targets (for .md links and repo paths).
export function inline(src, opts = {}) {
  const link = opts.link ?? (h => h);
  let out = '';
  let i = 0;
  const n = src.length;
  while (i < n) {
    const c = src[i];
    if (c === '\\' && i + 1 < n && /[\\`*_{}[\]()#+\-.!|<>~]/.test(src[i + 1])) {
      out += esc(src[i + 1]); i += 2; continue;
    }
    if (c === '`') {
      let ticks = 0;
      while (src[i + ticks] === '`') ticks++;
      const fence = '`'.repeat(ticks);
      const end = src.indexOf(fence, i + ticks);
      if (end > 0) {
        let code = src.slice(i + ticks, end);
        if (code.startsWith(' ') && code.endsWith(' ') && code.trim()) code = code.slice(1, -1);
        out += `<code>${esc(code)}</code>`;
        i = end + ticks; continue;
      }
      out += esc(fence); i += ticks; continue;
    }
    if (c === '<') {
      const m = /^<(https?:\/\/[^>\s]+)>/.exec(src.slice(i));
      if (m) { out += `<a href="${esc(link(m[1]))}">${esc(m[1])}</a>`; i += m[0].length; continue; }
      const t = /^<(\/?)(br|kbd|abbr|sup|sub|span)(\s[^>]*)?\/?>/.exec(src.slice(i));
      if (t) { out += t[0]; i += t[0].length; continue; }
    }
    if (c === '[') {
      const close = matchBracket(src, i);
      if (close > 0 && src[close + 1] === '(') {
        const end = src.indexOf(')', close + 2);
        if (end > 0) {
          const href = src.slice(close + 2, end).trim();
          out += `<a href="${esc(link(href))}">${inline(src.slice(i + 1, close), opts)}</a>`;
          i = end + 1; continue;
        }
      }
    }
    if ((c === '*' || c === '_') && src[i + 1] === c) {
      const end = src.indexOf(c + c, i + 2);
      if (end > i + 2) { out += `<strong>${inline(src.slice(i + 2, end), opts)}</strong>`; i = end + 2; continue; }
    }
    if (c === '*' || (c === '_' && (i === 0 || /\W/.test(src[i - 1])))) {
      const re = c === '*' ? /\*/g : /_(?!\w)/g;
      re.lastIndex = i + 1;
      const m = re.exec(src);
      if (m && m.index > i + 1 && src[i + 1] !== ' ') {
        out += `<em>${inline(src.slice(i + 1, m.index), opts)}</em>`; i = m.index + 1; continue;
      }
    }
    out += esc(c); i++;
  }
  return out;
}

function matchBracket(s, open) {
  let depth = 0;
  for (let i = open; i < s.length; i++) {
    if (s[i] === '\\') { i++; continue; }
    if (s[i] === '`') { const e = s.indexOf('`', i + 1); if (e > 0) { i = e; continue; } }
    if (s[i] === '[') depth++;
    else if (s[i] === ']' && --depth === 0) return i;
  }
  return -1;
}

// ---------------------------------------------------------------- blocks

const RAW_HTML = /^<(div|p|figure|section|aside|details|summary|table|ul|ol|dl|svg|nav|form|!--)[\s>]/i;

// Renders Markdown to HTML. Returns { html, headings: [{level, text, id}] }.
// opts.link(href) rewrites links; opts.code(info, text) may return custom HTML for a fence
// (return null for the default); opts.idPrefix avoids clashes with page ids.
export function render(md, opts = {}) {
  const lines = md.replace(/\r\n?/g, '\n').replace(/\t/g, '    ').split('\n');
  const headings = [];
  const used = new Set(opts.reservedIds ?? []);
  const uniq = id => { let u = id, k = 2; while (used.has(u)) u = `${id}-${k++}`; used.add(u); return u; };
  const html = blocks(lines, 0, lines.length, opts, headings, uniq);
  return { html, headings };
}

function blocks(lines, start, end, opts, headings, uniq) {
  const out = [];
  let i = start;
  while (i < end) {
    const line = lines[i];
    if (!line.trim()) { i++; continue; }

    // Fenced code
    const fence = /^(\s*)(`{3,}|~{3,})(.*)$/.exec(line);
    if (fence) {
      const [, indent, marker, info] = fence;
      const body = [];
      i++;
      while (i < end && !(lines[i].trim().startsWith(marker) && lines[i].trim().replace(/[`~]/g, '') === '')) {
        body.push(lines[i].startsWith(indent) ? lines[i].slice(indent.length) : lines[i].trimStart());
        i++;
      }
      i++;
      const text = body.join('\n');
      const custom = opts.code?.(info.trim(), text);
      if (custom != null) { out.push(custom); continue; }
      const lang = info.trim().split(/\s+/)[0];
      out.push(`<pre class="code"${lang ? ` data-lang="${esc(lang)}"` : ''}><code>${esc(text)}</code></pre>`);
      continue;
    }

    // Heading
    const h = /^(#{1,6})\s+(.*?)\s*#*\s*$/.exec(line);
    if (h) {
      const level = h[1].length;
      let text = h[2];
      let id = null;
      const idm = /\s*\{#([\w-]+)\}$/.exec(text);
      if (idm) { id = idm[1]; text = text.slice(0, idm.index); }
      const inner = inline(text, opts);
      id = uniq(id ?? slugify(inner));
      headings.push({ level, text: inner.replace(/<[^>]+>/g, ''), html: inner, id });
      out.push(level === 1
        ? `<h1 id="${id}">${inner}</h1>`
        : `<h${level} id="${id}"><a class="anchor" href="#${id}" aria-label="Link to this section"><span aria-hidden="true">#</span></a>${inner}</h${level}>`);
      i++; continue;
    }

    // Rule
    if (/^\s*(-{3,}|\*{3,})\s*$/.test(line)) { out.push('<hr>'); i++; continue; }

    // Raw HTML block: until a blank line
    if (RAW_HTML.test(line.trim())) {
      const body = [];
      while (i < end && lines[i].trim()) body.push(lines[i++]);
      out.push(body.join('\n'));
      continue;
    }

    // Blockquote
    if (/^\s*>/.test(line)) {
      const body = [];
      while (i < end && /^\s*>/.test(lines[i])) body.push(lines[i++].replace(/^\s*> ?/, ''));
      out.push(`<blockquote>${blocks(body, 0, body.length, opts, headings, uniq)}</blockquote>`);
      continue;
    }

    // Table
    if (line.includes('|') && i + 1 < end && /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(lines[i + 1])) {
      const cells = l => splitRow(l);
      const head = cells(line);
      const align = cells(lines[i + 1]).map(c => c.startsWith(':') && c.endsWith(':') ? 'center' : c.endsWith(':') ? 'num' : '');
      i += 2;
      const rows = [];
      while (i < end && lines[i].includes('|') && lines[i].trim()) rows.push(cells(lines[i++]));
      const cls = k => align[k] ? ` class="${align[k]}"` : '';
      out.push(`<div class="table-wrap"><table>
<thead><tr>${head.map((c, k) => `<th scope="col"${cls(k)}>${inline(c, opts)}</th>`).join('')}</tr></thead>
<tbody>
${rows.map(r => `<tr>${head.map((_, k) => `<td${cls(k)}>${inline(r[k] ?? '', opts)}</td>`).join('')}</tr>`).join('\n')}
</tbody>
</table></div>`);
      continue;
    }

    // List
    const li = /^(\s*)([-*]|\d+[.)])\s+/.exec(line);
    if (li) {
      const [list, next] = listBlock(lines, i, end, opts, headings, uniq);
      out.push(list); i = next; continue;
    }

    // Paragraph
    const para = [];
    while (i < end && lines[i].trim() && !startsBlock(lines, i)) para.push(lines[i++].trim());
    out.push(`<p>${inline(para.join(' '), opts)}</p>`);
  }
  return out.join('\n');
}

function splitRow(l) {
  let s = l.trim();
  if (s.startsWith('|')) s = s.slice(1);
  if (s.endsWith('|') && !s.endsWith('\\|')) s = s.slice(0, -1);
  const cells = [];
  let cur = '', inCode = false;
  for (let k = 0; k < s.length; k++) {
    const ch = s[k];
    if (ch === '\\' && s[k + 1] === '|') { cur += '|'; k++; continue; }
    if (ch === '`') inCode = !inCode;
    if (ch === '|' && !inCode) { cells.push(cur.trim()); cur = ''; continue; }
    cur += ch;
  }
  cells.push(cur.trim());
  return cells;
}

function startsBlock(lines, i) {
  const l = lines[i];
  return /^(\s*)(`{3,}|~{3,})/.test(l) || /^#{1,6}\s/.test(l) || /^\s*>/.test(l) ||
    /^\s*([-*]|\d+[.)])\s+/.test(l) || /^\s*(-{3,}|\*{3,})\s*$/.test(l) || RAW_HTML.test(l.trim()) ||
    (l.includes('|') && i + 1 < lines.length && /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(lines[i + 1]));
}

function listBlock(lines, start, end, opts, headings, uniq) {
  const first = /^(\s*)([-*]|\d+[.)])\s+/.exec(lines[start]);
  const indent = first[1].length;
  const ordered = /\d/.test(first[2]);
  const startNum = ordered ? parseInt(first[2], 10) : 1;
  const items = [];
  let i = start;
  let loose = false;
  while (i < end) {
    const m = /^(\s*)([-*]|\d+[.)])\s+(.*)$/.exec(lines[i]);
    if (!m || m[1].length !== indent || /\d/.test(m[2]) !== ordered) break;
    const contentIndent = m[1].length + m[2].length + 1;
    const body = [m[3]];
    i++;
    while (i < end) {
      const l = lines[i];
      if (!l.trim()) {
        // A blank line continues the item only if the next line is indented into it.
        if (i + 1 < end && lines[i + 1].trim() && leading(lines[i + 1]) >= contentIndent) { body.push(''); i++; loose = true; continue; }
        break;
      }
      if (leading(l) <= indent && /^\s*([-*]|\d+[.)])\s+/.test(l)) break;
      if (leading(l) < contentIndent && startsBlock(lines, i) && !/^\s*([-*]|\d+[.)])\s+/.test(l)) break;
      body.push(leading(l) >= contentIndent ? l.slice(contentIndent) : l.trim());
      i++;
    }
    items.push(body);
    if (i < end && !lines[i].trim()) {
      const nx = i + 1 < end ? /^(\s*)([-*]|\d+[.)])\s+/.exec(lines[i + 1]) : null;
      if (nx && nx[1].length === indent && /\d/.test(nx[2]) === ordered) { loose = true; i++; }
    }
  }
  const tag = ordered ? 'ol' : 'ul';
  const startAttr = ordered && startNum !== 1 ? ` start="${startNum}"` : '';
  const html = `<${tag}${startAttr}>\n${items.map(body => {
    const inner = blocks(body, 0, body.length, opts, headings, uniq);
    const tight = !loose && inner.startsWith('<p>') ? inner.replace(/^<p>([\s\S]*?)<\/p>/, '$1') : inner;
    return `<li>${tight}</li>`;
  }).join('\n')}\n</${tag}>`;
  return [html, i];
}

const leading = l => l.length - l.trimStart().length;
