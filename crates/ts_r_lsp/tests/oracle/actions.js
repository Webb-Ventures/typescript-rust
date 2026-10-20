// @ts-check
// Oracle for the editor-action fixtures (tests/fixtures/actions/*.txt): runs each fixture
// through tsserver and writes what it produces next to the fixture as `<name>.tsserver`.
// tests/actions.rs compares ts-r's output with these files. tsserver is only run (an oracle),
// never read.
//
//   scripts/limit.sh -- node crates/ts_r_lsp/tests/oracle/actions.js <typescript/lib dir> [name...]
//
// Fixture format: see tests/actions.rs.
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const childProcess = require('child_process');

const [libDir, ...only] = process.argv.slice(2);
if (!libDir) {
    console.error('usage: actions.js <typescript/lib dir> [fixture name...]');
    process.exit(2);
}
const fixtures = path.join(__dirname, '..', 'fixtures', 'actions');
const tsserverJs = path.join(libDir, 'tsserver.js');
const version = JSON.parse(fs.readFileSync(path.join(libDir, '..', 'package.json'), 'utf8')).version;

const DEFAULT_TSCONFIG = JSON.stringify({
    compilerOptions: { strict: true, target: 'es2022', module: 'esnext', moduleResolution: 'bundler', noEmit: true, jsx: 'react-jsx' },
});

/** Parses a fixture: directives, files (markers removed), cursor/range. */
function parseFixture(/** @type {string} */ text) {
    /** @type {Record<string, string>} */
    const dir = {};
    /** @type {{name: string, text: string}[]} */
    const files = [];
    let cur = null;
    for (const line of text.split('\n')) {
        const m = /^\/\/ @(\w+):\s?(.*)$/.exec(line);
        if (m && m[1] === 'file') {
            cur = { name: m[2].trim(), text: '' };
            files.push(cur);
            continue;
        }
        if (m && !cur) {
            dir[m[1]] = m[2];
            continue;
        }
        if (cur) cur.text += line + '\n';
    }
    // The last file's trailing newline comes from the fixture's own final newline.
    for (const f of files) if (f.text.endsWith('\n\n')) f.text = f.text.slice(0, -1);
    /** @type {{file: string, start: number, end: number} | null} */
    let sel = null;
    for (const f of files) {
        const a = f.text.indexOf('/*[*/');
        if (a >= 0) {
            f.text = f.text.replace('/*[*/', '');
            const b = f.text.indexOf('/*]*/');
            f.text = f.text.replace('/*]*/', '');
            sel = { file: f.name, start: a, end: b };
        }
        const c = f.text.indexOf('/*|*/');
        if (c >= 0) {
            f.text = f.text.replace('/*|*/', '');
            sel = { file: f.name, start: c, end: c };
        }
    }
    return { dir, files, sel };
}

class TsServer {
    constructor(/** @type {string} */ cwd) {
        this.seq = 0;
        /** @type {Map<number, (m: any) => void>} */
        this.pending = new Map();
        this.buffer = Buffer.alloc(0);
        this.child = childProcess.spawn(process.execPath, ['--max-old-space-size=2048', tsserverJs, '--disableAutomaticTypingAcquisition'], {
            cwd,
            stdio: ['pipe', 'pipe', 'ignore'],
        });
        this.child.stdout.on('data', (/** @type {Buffer} */ c) => this.onData(c));
    }
    onData(/** @type {Buffer} */ chunk) {
        this.buffer = Buffer.concat([this.buffer, chunk]);
        for (;;) {
            const h = this.buffer.indexOf('\r\n\r\n');
            if (h < 0) return;
            const m = /Content-Length:\s*(\d+)/i.exec(this.buffer.subarray(0, h).toString('ascii'));
            const len = m ? Number(m[1]) : 0;
            if (this.buffer.length < h + 4 + len) return;
            const body = this.buffer.subarray(h + 4, h + 4 + len).toString('utf8');
            this.buffer = this.buffer.subarray(h + 4 + len);
            const msg = JSON.parse(body);
            if (msg.type === 'response') {
                const p = this.pending.get(msg.request_seq);
                if (p) {
                    this.pending.delete(msg.request_seq);
                    p(msg);
                }
            }
        }
    }
    /** @returns {Promise<any>} */
    request(/** @type {string} */ command, /** @type {any} */ args) {
        const seq = ++this.seq;
        this.child.stdin.write(JSON.stringify({ seq, type: 'request', command, arguments: args }) + '\n');
        return new Promise((resolve, reject) => {
            const t = setTimeout(() => reject(new Error(`${command} timed out`)), 60000);
            this.pending.set(seq, (m) => {
                clearTimeout(t);
                resolve(m);
            });
        });
    }
    stop() {
        this.child.stdin.write(JSON.stringify({ seq: ++this.seq, type: 'request', command: 'exit' }) + '\n');
        setTimeout(() => this.child.kill(), 500);
    }
}

/** 1-based line/offset (UTF-16) of a JS string index. */
function lineOffset(/** @type {string} */ text, /** @type {number} */ i) {
    const before = text.slice(0, i).split('\n');
    return { line: before.length, offset: before[before.length - 1].length + 1 };
}

function indexOf(/** @type {string} */ text, /** @type {{line: number, offset: number}} */ p) {
    const lines = text.split('\n');
    let i = 0;
    for (let l = 0; l < p.line - 1 && l < lines.length; l++) i += lines[l].length + 1;
    return i + p.offset - 1;
}

/** Applies tsserver FileCodeEdits to the in-memory files. */
function apply(/** @type {Map<string, string>} */ texts, /** @type {any[]} */ changes, /** @type {string} */ root) {
    for (const fc of changes) {
        const name = path.relative(root, fc.fileName).split(path.sep).join('/');
        let t = texts.get(name) ?? '';
        const edits = fc.textChanges
            .map((/** @type {any} */ e) => ({ s: indexOf(t, e.start), e: indexOf(t, e.end), text: e.newText }))
            .sort((/** @type {any} */ a, /** @type {any} */ b) => b.s - a.s || b.e - a.e);
        for (const e of edits) t = t.slice(0, e.s) + e.text + t.slice(e.e);
        texts.set(name, t);
    }
}

function render(/** @type {Map<string, string>} */ before, /** @type {Map<string, string>} */ after) {
    const names = [...after.keys()].filter((n) => before.get(n) !== after.get(n)).sort();
    const gone = [...before.keys()].filter((n) => !after.has(n)).sort();
    if (!names.length && !gone.length) return '// no change\n';
    let out = '';
    for (const n of gone) out += `// @deleted: ${n}\n`;
    for (const n of names) out += `// @file: ${n}\n${after.get(n)}`;
    if (!out.endsWith('\n')) out += '\n';
    return out;
}

async function runOne(/** @type {string} */ name) {
    const fx = parseFixture(fs.readFileSync(path.join(fixtures, `${name}.txt`), 'utf8'));
    const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'tsr-actions-oracle-')));
    /** @type {Map<string, string>} */
    const texts = new Map();
    // A fixture with its own tsconfig(s) (possibly several projects) gets no default one.
    const hasConfig = fx.files.some((f) => path.basename(f.name) === 'tsconfig.json');
    if (!hasConfig) fs.writeFileSync(path.join(root, 'tsconfig.json'), fx.dir.tsconfig ?? DEFAULT_TSCONFIG);
    for (const f of fx.files) {
        const p = path.join(root, f.name);
        fs.mkdirSync(path.dirname(p), { recursive: true });
        fs.writeFileSync(p, f.text);
        texts.set(f.name, f.text);
    }
    // `@symlinks: <link> -> <target>, ...` (paths relative to the root; workspace packages).
    for (const pair of (fx.dir.symlinks ?? '').split(',').map((x) => x.trim()).filter(Boolean)) {
        const [link, target] = pair.split('->').map((x) => x.trim());
        const l = path.join(root, link);
        fs.mkdirSync(path.dirname(l), { recursive: true });
        fs.symlinkSync(path.relative(path.dirname(l), path.join(root, target)), l);
    }
    const before = new Map(texts);
    const s = new TsServer(root);
    try {
        await s.request('configure', {
            preferences: {
                quotePreference: 'auto',
                importModuleSpecifierPreference: 'shortest',
                importModuleSpecifierEnding: 'auto',
                includePackageJsonAutoImports: 'off',
                provideRefactorNotApplicableReason: true,
                allowRenameOfImportPath: true,
                // As VS Code sends it.
                allowTextChangesInNewFiles: true,
                ...(fx.dir.preferences ? JSON.parse(fx.dir.preferences) : {}),
            },
            ...(fx.dir.format ? { formatOptions: JSON.parse(fx.dir.format) } : {}),
        });
        const main = fx.sel ? fx.sel.file : fx.files[0].name;
        const mainPath = path.join(root, main);
        // `@open: <file> <file>...`: only these are open (default: every source file).
        const open = fx.dir.open ? fx.dir.open.trim().split(/\s+/) : fx.files.map((f) => f.name).filter((n) => !n.endsWith('.json'));
        await s.request('updateOpen', { openFiles: open.map((n) => ({ file: path.join(root, n), projectRootPath: root })) });
        const action = (fx.dir.action ?? '').trim();
        const [verb, ...rest] = action.split(' ');
        const arg = rest.join(' ');
        const mainText = texts.get(main) ?? '';
        const sel = fx.sel ?? { file: main, start: 0, end: 0 };
        const a = lineOffset(mainText, sel.start);
        const b = lineOffset(mainText, sel.end);
        let changes = null;
        let note = '';
        if (verb === 'organizeImports') {
            const r = await s.request('organizeImports', { scope: { type: 'file', args: { file: mainPath } }, mode: arg || undefined });
            changes = r.body;
        } else if (verb === 'fix' || verb === 'fixAll') {
            // `fix <code> <title>` / `fixAll <code> <title of the fix whose fixId to use>`.
            const [codeStr, ...t] = arg.split(' ');
            const code = Number(codeStr);
            const title = t.join(' ');
            const diags = [
                ...((await s.request('semanticDiagnosticsSync', { file: mainPath })).body ?? []),
                ...((await s.request('suggestionDiagnosticsSync', { file: mainPath })).body ?? []),
                ...((await s.request('syntacticDiagnosticsSync', { file: mainPath })).body ?? []),
            ];
            const d = diags.find(
                (x) => x.code === code && indexOf(mainText, x.start) <= sel.start && sel.start <= indexOf(mainText, x.end),
            );
            if (!d) {
                note = `// no diagnostic ${code} at the cursor\n`;
            } else {
                const r = await s.request('getCodeFixes', {
                    file: mainPath,
                    startLine: d.start.line,
                    startOffset: d.start.offset,
                    endLine: d.end.line,
                    endOffset: d.end.offset,
                    errorCodes: [code],
                });
                const fixes = r.body ?? [];
                if (process.env.TS_R_ORACLE_FIXNAMES) {
                    for (const f of fixes) console.error(`fixName ${f.fixName} fixId ${f.fixId ?? '-'} ${JSON.stringify(f.description)}`);
                }
                const fix = fixes.find((/** @type {any} */ f) => f.description === title);
                if (!fix) {
                    note = `// no fix titled ${JSON.stringify(title)}; offered: ${JSON.stringify(fixes.map((/** @type {any} */ f) => f.description))}\n`;
                } else if (verb === 'fix') {
                    changes = fix.changes;
                } else if (!fix.fixId) {
                    note = `// fix ${JSON.stringify(title)} has no fix-all\n`;
                } else {
                    const c = await s.request('getCombinedCodeFix', { scope: { type: 'file', args: { file: mainPath } }, fixId: fix.fixId });
                    changes = c.body?.changes;
                    note = `// fix all: ${fix.fixAllDescription}\n`;
                }
            }
        } else if (verb === 'refactor') {
            // `refactor <refactor name>/<action name>` (tsserver's names).
            const slash = arg.lastIndexOf('/');
            const refactor = arg.slice(0, slash);
            const actionName = arg.slice(slash + 1);
            const span = { file: mainPath, startLine: a.line, startOffset: a.offset, endLine: b.line, endOffset: b.offset };
            const target = fx.dir.target ? fx.dir.target.trim() : '';
            const ap = await s.request('getApplicableRefactors', {
                ...span,
                triggerReason: (fx.dir.trigger ?? '').trim() === 'implicit' ? 'implicit' : 'invoked',
                ...(target ? { includeInteractiveActions: true } : {}),
            });
            const offered = (ap.body ?? []).flatMap((/** @type {any} */ r) => r.actions.filter((/** @type {any} */ x) => !x.notApplicableReason).map((/** @type {any} */ x) => `${r.name}/${x.name}`));
            if (!offered.includes(`${refactor}/${actionName}`)) {
                note = `// not offered; offered: ${JSON.stringify(offered)}\n`;
            } else {
                const r = await s.request('getEditsForRefactor', {
                    ...span,
                    refactor,
                    action: actionName,
                    ...(target ? { interactiveRefactorArguments: { targetFile: path.join(root, target) } } : {}),
                });
                changes = r.body?.edits;
                const chosen = (ap.body ?? []).flatMap((/** @type {any} */ x) => (x.name === refactor ? x.actions : [])).find((/** @type {any} */ x) => x.name === actionName && !x.notApplicableReason);
                note = `// title: ${chosen?.description ?? ''}\n`;
            }
        } else if (verb === 'rename') {
            const [from, to] = arg.split(' ');
            const r = await s.request('getEditsForFileRename', { oldFilePath: path.join(root, from), newFilePath: path.join(root, to) });
            changes = r.body;
            // The client moves the file, or every file under the folder.
            const moved = (/** @type {string} */ n) => (n === from ? to : n.startsWith(from + '/') ? to + n.slice(from.length) : null);
            for (const n of [...texts.keys()]) {
                const m = moved(n);
                if (m === null) continue;
                const t = /** @type {string} */ (texts.get(n));
                texts.delete(n);
                texts.set(m, t);
            }
            for (const c of changes ?? []) {
                const m = moved(path.relative(root, c.fileName).split(path.sep).join('/'));
                if (m !== null) c.fileName = path.join(root, m);
            }
        } else if (verb === 'moveSuggestions') {
            const span = { file: mainPath, startLine: a.line, startOffset: a.offset, endLine: b.line, endOffset: b.offset };
            const r = (await s.request('getMoveToRefactoringFileSuggestions', span)).body ?? {};
            const rel = (/** @type {string | undefined} */ p) => (p ? path.relative(root, p).split(path.sep).join('/') : '');
            return `// newFileName: ${rel(r.newFileName)}\n// files: ${(r.files ?? []).map(rel).join(' ')}\n`;
        } else {
            note = `// unknown action ${JSON.stringify(action)}\n`;
        }
        if (changes) {
            for (const c of changes) {
                const n = path.relative(root, c.fileName).split(path.sep).join('/');
                if (!texts.has(n) && c.isNewFile) texts.set(n, '');
            }
            apply(texts, changes, root);
        }
        return note + (changes || !note ? render(before, texts) : '');
    } finally {
        s.stop();
        fs.rmSync(root, { recursive: true, force: true });
    }
}

async function main() {
    const names = only.length
        ? only
        : fs
              .readdirSync(fixtures)
              .filter((f) => f.endsWith('.txt'))
              .map((f) => f.slice(0, -4))
              .sort();
    for (const n of names) {
        try {
            const out = await runOne(n);
            fs.writeFileSync(path.join(fixtures, `${n}.tsserver`), `// tsserver ${version}\n${out}`);
            console.log(`${n}: ${out.split('\n')[0]}`);
        } catch (e) {
            console.error(`${n}: ${e}`);
        }
    }
}

main();
