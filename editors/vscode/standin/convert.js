// @ts-check
// tsserver protocol <-> LSP conversions for the tsserver bridge (router.js): VS Code's
// TypeScript extension asks in tsserver terms (1-based line/offset, file paths), the ts-r
// checker answers in LSP terms (0-based line/character, URIs). Both count UTF-16 code units,
// like JavaScript strings. No VS Code or Node process dependencies, so `npm test` covers it.
'use strict';

const url = require('url');

/** @typedef {{line: number, offset: number}} TsPos */
/** @typedef {{line: number, character: number}} LspPos */
/** @typedef {{start: LspPos, end: LspPos}} LspRange */

/** @param {TsPos} p @returns {LspPos} */
function toLsp(p) {
    return { line: Math.max(0, p.line - 1), character: Math.max(0, p.offset - 1) };
}

/** @param {LspPos} p @returns {TsPos} */
function toTs(p) {
    return { line: p.line + 1, offset: p.character + 1 };
}

/** @param {LspRange} r */
function spanOf(r) {
    return { start: toTs(r.start), end: toTs(r.end) };
}

/** @param {string} file */
function pathToUri(file) {
    return url.pathToFileURL(file).href;
}

/** @param {string} uri */
function uriToPath(uri) {
    try {
        return url.fileURLToPath(uri);
    } catch {
        return uri;
    }
}

/** A document's text with a line index (offsets are UTF-16 code units, as in JS strings). */
class TextDoc {
    /** @param {string} text */
    constructor(text) {
        this.text = text;
        /** @type {number[] | null} */
        this.starts = null;
    }

    lineStarts() {
        if (this.starts) return this.starts;
        const starts = [0];
        const t = this.text;
        for (let i = 0; i < t.length; i++) {
            const c = t.charCodeAt(i);
            if (c === 13) {
                if (t.charCodeAt(i + 1) === 10) i++;
                starts.push(i + 1);
            } else if (c === 10) {
                starts.push(i + 1);
            }
        }
        this.starts = starts;
        return starts;
    }

    /** Index of a 0-based line and character, clamped to the text. */
    indexOf(/** @type {number} */ line, /** @type {number} */ character) {
        const starts = this.lineStarts();
        if (line >= starts.length) return this.text.length;
        const start = starts[Math.max(0, line)];
        const end = line + 1 < starts.length ? starts[line + 1] : this.text.length;
        return Math.min(start + Math.max(0, character), end);
    }

    /** @param {TsPos} p */
    indexOfTs(p) {
        return this.indexOf(p.line - 1, p.offset - 1);
    }

    /** @param {number} index @returns {TsPos} */
    tsPos(index) {
        const starts = this.lineStarts();
        let lo = 0;
        let hi = starts.length - 1;
        while (lo < hi) {
            const mid = (lo + hi + 1) >> 1;
            if (starts[mid] <= index) lo = mid;
            else hi = mid - 1;
        }
        return { line: lo + 1, offset: index - starts[lo] + 1 };
    }

    /**
     * Applies tsserver `textChanges` (VS Code sends them so that applying them last to first,
     * as tsserver does, gives the new text) and returns the same edits as LSP content changes
     * in application order.
     * @param {{start: TsPos, end: TsPos, newText: string}[]} changes
     * @returns {{range: LspRange, text: string}[]}
     */
    applyTsChanges(changes) {
        /** @type {{range: LspRange, text: string}[]} */
        const out = [];
        for (let i = changes.length - 1; i >= 0; i--) {
            const c = changes[i];
            const s = this.indexOfTs(c.start);
            const e = Math.max(s, this.indexOfTs(c.end));
            this.text = this.text.slice(0, s) + c.newText + this.text.slice(e);
            this.starts = null;
            out.push({ range: { start: toLsp(c.start), end: toLsp(c.end) }, text: c.newText });
        }
        return out;
    }

    /** The identifier around a tsserver position (for bound spans and symbol names). */
    wordAt(/** @type {TsPos} */ p) {
        const i = this.indexOfTs(p);
        const word = /[\p{ID_Continue}$‌‍#@]/u;
        let s = i;
        let e = i;
        while (s > 0 && word.test(this.text[s - 1])) s--;
        while (e < this.text.length && word.test(this.text[e])) e++;
        return { text: this.text.slice(s, e), start: this.tsPos(s), end: this.tsPos(e) };
    }

    /** End position of the text. */
    endPos() {
        return this.tsPos(this.text.length);
    }
}

/** tsserver `scriptKindName` to an LSP language id. */
function languageId(/** @type {string | undefined} */ scriptKindName, /** @type {string} */ file) {
    switch (scriptKindName) {
        case 'TSX':
            return 'typescriptreact';
        case 'JS':
            return 'javascript';
        case 'JSX':
            return 'javascriptreact';
        case 'TS':
            return 'typescript';
        default:
            if (/\.tsx$/i.test(file)) return 'typescriptreact';
            if (/\.jsx$/i.test(file)) return 'javascriptreact';
            if (/\.[cm]?js$/i.test(file)) return 'javascript';
            return 'typescript';
    }
}

// ---------------------------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------------------------

/** @param {number | undefined} severity */
function category(severity) {
    switch (severity) {
        case 2:
            return 'warning';
        case 3:
            return 'message';
        case 4:
            return 'suggestion';
        default:
            return 'error';
    }
}

/**
 * LSP diagnostics of one file as tsserver `syntaxDiag` / `semanticDiag` / `suggestionDiag`
 * bodies' diagnostics. `source` is set to `ts-r` so the Problems panel shows which engine
 * reported them.
 * @param {any[]} diagnostics
 */
function splitDiagnostics(diagnostics) {
    /** @type {{syntax: any[], semantic: any[], suggestion: any[]}} */
    const out = { syntax: [], semantic: [], suggestion: [] };
    for (const d of diagnostics || []) {
        // ts-r's own, or TypeScript's (`ts`: a fallback project's, forwarded by ts-r); lint and
        // others have their own channel.
        if (d.source && d.source !== 'ts-r' && d.source !== 'ts') continue;
        /** @type {any} */
        const t = {
            start: toTs(d.range.start),
            end: toTs(d.range.end),
            text: d.message,
            code: typeof d.code === 'number' ? d.code : Number(String(d.code ?? '').replace(/^TS/i, '')) || 0,
            category: category(d.severity),
            source: d.source === 'ts' ? 'ts' : 'ts-r',
        };
        const tags = d.tags || [];
        if (tags.includes(1)) t.reportsUnnecessary = true;
        if (tags.includes(2)) t.reportsDeprecated = true;
        if (Array.isArray(d.relatedInformation) && d.relatedInformation.length) {
            t.relatedInformation = d.relatedInformation.map((/** @type {any} */ r) => ({
                span: { ...spanOf(r.location.range), file: uriToPath(r.location.uri) },
                message: r.message,
                category: 'message',
                code: 0,
            }));
        }
        if (d.data?.kind === 'syntax') out.syntax.push(t);
        else if (d.severity === 4) out.suggestion.push(t);
        else out.semantic.push(t);
    }
    return out;
}

// ---------------------------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------------------------

/** LSP `CompletionItemKind` to tsserver's `ScriptElementKind`. */
const COMPLETION_KIND = /** @type {Record<number, string>} */ ({
    1: 'string',
    2: 'method',
    3: 'function',
    4: 'constructor',
    5: 'property',
    6: 'var',
    7: 'class',
    8: 'interface',
    9: 'module',
    10: 'property',
    11: 'var',
    12: 'var',
    13: 'enum',
    14: 'keyword',
    15: 'var',
    16: 'var',
    17: 'script',
    18: 'var',
    19: 'directory',
    20: 'enum member',
    21: 'const',
    22: 'type',
    23: 'var',
    24: 'var',
    25: 'type parameter',
});

/** LSP `SymbolKind` to tsserver's `ScriptElementKind` (navigation tree). */
const SYMBOL_KIND = /** @type {Record<number, string>} */ ({
    1: 'script',
    2: 'module',
    3: 'module',
    4: 'module',
    5: 'class',
    6: 'method',
    7: 'property',
    8: 'property',
    9: 'constructor',
    10: 'enum',
    11: 'interface',
    12: 'function',
    13: 'var',
    14: 'const',
    15: 'var',
    16: 'var',
    17: 'var',
    18: 'var',
    19: 'var',
    20: 'property',
    21: 'var',
    22: 'enum member',
    23: 'type',
    24: 'var',
    25: 'var',
    26: 'type parameter',
});

/** `textDocument/hover` result to a `quickinfo` body (undefined: nothing to show). */
function quickInfo(/** @type {any} */ hover, /** @type {TsPos} */ at) {
    if (!hover || !hover.contents) return undefined;
    const c = hover.contents;
    const value = typeof c === 'string' ? c : Array.isArray(c) ? c.map((x) => (typeof x === 'string' ? x : x.value)).join('\n\n') : c.value || '';
    let displayString = '';
    let documentation = value;
    const m = /^```(?:typescript|ts)?\n([\s\S]*?)\n```\n?([\s\S]*)$/.exec(value);
    if (m) {
        displayString = m[1];
        documentation = m[2].trim();
    }
    const range = hover.range ? spanOf(hover.range) : { start: at, end: at };
    return { kind: '', kindModifiers: '', ...range, displayString, documentation, tags: [] };
}

/**
 * `textDocument/completion` result to a `completionInfo` body. The LSP item travels in each
 * entry's `data` and comes back in `completionEntryDetails`.
 * @param {any} result
 * @param {{isMemberCompletion: boolean}} ctx
 */
function completionInfo(result, ctx) {
    const items = Array.isArray(result) ? result : result?.items || [];
    const entries = items.map((/** @type {any} */ item) => {
        /** @type {any} */
        const e = {
            name: item.label,
            kind: COMPLETION_KIND[item.kind] || 'var',
            kindModifiers: '',
            sortText: item.sortText || '11',
            data: { tsr: item },
        };
        if (item.filterText) e.filterText = item.filterText;
        if (item.textEdit) {
            const range = item.textEdit.range || item.textEdit.replace;
            e.insertText = item.textEdit.newText;
            if (range) e.replacementSpan = spanOf(range);
        } else if (item.insertText && item.insertText !== item.label) {
            e.insertText = item.insertText;
        }
        if (item.insertTextFormat === 2) e.isSnippet = true;
        if (Array.isArray(item.additionalTextEdits) && item.additionalTextEdits.length) {
            e.hasAction = true;
            const spec = item.labelDetails?.description;
            if (spec) {
                e.source = spec;
                e.sourceDisplay = [{ text: spec, kind: 'text' }];
            }
        }
        if (item.labelDetails?.description || item.labelDetails?.detail) e.labelDetails = item.labelDetails;
        return e;
    });
    return {
        isGlobalCompletion: !ctx.isMemberCompletion,
        isMemberCompletion: ctx.isMemberCompletion,
        isNewIdentifierLocation: false,
        isIncomplete: Array.isArray(result) ? undefined : result?.isIncomplete || undefined,
        entries,
    };
}

/**
 * A resolved LSP completion item to a `completionEntryDetails` entry.
 * @param {any} item resolved item
 * @param {string} file the file being edited (auto-import edits go there)
 */
function entryDetails(item, file) {
    const docs = typeof item.documentation === 'string' ? item.documentation : item.documentation?.value || '';
    /** @type {any} */
    const d = {
        name: item.label,
        kind: COMPLETION_KIND[item.kind] || 'var',
        kindModifiers: '',
        displayParts: item.detail ? [{ text: item.detail, kind: 'text' }] : [],
        documentation: docs ? [{ text: docs, kind: 'text' }] : [],
        tags: [],
    };
    const edits = item.additionalTextEdits || [];
    if (edits.length) {
        d.codeActions = [
            {
                description: item.detail || 'Add import',
                changes: [
                    {
                        fileName: file,
                        textChanges: edits.map((/** @type {any} */ t) => ({ ...spanOf(t.range), newText: t.newText })),
                    },
                ],
            },
        ];
    }
    const spec = item.labelDetails?.description;
    if (spec) {
        d.source = [{ text: spec, kind: 'text' }];
        d.sourceDisplay = [{ text: spec, kind: 'text' }];
    }
    return d;
}

/** Location | Location[] | LocationLink[] to tsserver file spans. */
function fileSpans(/** @type {any} */ result) {
    const list = !result ? [] : Array.isArray(result) ? result : [result];
    return list.map((/** @type {any} */ l) => {
        if (l.targetUri) {
            /** @type {any} */
            const span = { file: uriToPath(l.targetUri), ...spanOf(l.targetSelectionRange || l.targetRange) };
            if (l.targetRange) {
                span.contextStart = toTs(l.targetRange.start);
                span.contextEnd = toTs(l.targetRange.end);
            }
            return span;
        }
        return { file: uriToPath(l.uri), ...spanOf(l.range) };
    });
}

/** @param {{file: string, start: TsPos, end: TsPos}} a @param {{file: string, start: TsPos, end: TsPos}} b */
function sameSpan(a, b) {
    return a.file === b.file && a.start.line === b.start.line && a.start.offset === b.start.offset;
}

/**
 * `textDocument/references` (+ the definitions, to mark `isDefinition`) to a `references` body.
 * @param {any} refs
 * @param {any} defs
 * @param {{text: string, start: TsPos}} word
 */
function referencesBody(refs, defs, word) {
    const definitions = fileSpans(defs);
    return {
        refs: fileSpans(refs).map((r) => ({
            ...r,
            lineText: '',
            isWriteAccess: false,
            isDefinition: definitions.some((d) => sameSpan(d, r)),
        })),
        symbolName: word.text,
        symbolStartOffset: word.start.offset,
        symbolDisplayString: '',
    };
}

/** `textDocument/documentHighlight` to a `documentHighlights` body. */
function highlightsBody(/** @type {any[] | null} */ result, /** @type {string} */ file) {
    if (!result || result.length === 0) return [];
    return [
        {
            file,
            highlightSpans: result.map((h) => ({ ...spanOf(h.range), kind: h.kind === 3 ? 'writtenReference' : 'reference' })),
        },
    ];
}

/** Placeholder name for `textDocument/rename` (tsserver's `rename` has no new name). */
const RENAME_PLACEHOLDER = 'tsrRenamePlaceholder';

/**
 * `prepareRename` + `rename` (with the placeholder name) to a tsserver `rename` body.
 * @param {any} prepared
 * @param {any} edit WorkspaceEdit
 * @param {{text: string, start: TsPos, end: TsPos}} word
 */
function renameBody(prepared, edit, word) {
    if (!prepared || !edit) {
        return { info: { canRename: false, localizedErrorMessage: 'You cannot rename this element.' }, locs: [] };
    }
    const range = prepared.range || (prepared.start ? prepared : null);
    const triggerSpan = range ? spanOf(range) : { start: word.start, end: word.end };
    const name = prepared.placeholder || word.text;
    /** @type {Map<string, any[]>} */
    const byFile = new Map();
    /** @param {string} uri @param {any[]} edits */
    const add = (uri, edits) => {
        const file = uriToPath(uri);
        const locs = byFile.get(file) || [];
        for (const t of edits || []) {
            /** @type {any} */
            const loc = spanOf(t.range);
            const i = t.newText.indexOf(RENAME_PLACEHOLDER);
            if (i < 0) continue;
            if (i > 0) loc.prefixText = t.newText.slice(0, i);
            const suffix = t.newText.slice(i + RENAME_PLACEHOLDER.length);
            if (suffix) loc.suffixText = suffix;
            locs.push(loc);
        }
        byFile.set(file, locs);
    };
    for (const [uri, edits] of Object.entries(edit.changes || {})) add(uri, /** @type {any[]} */ (edits));
    for (const dc of edit.documentChanges || []) if (dc.textDocument) add(dc.textDocument.uri, dc.edits);
    return {
        info: { canRename: true, displayName: name, fullDisplayName: name, kind: '', kindModifiers: '', triggerSpan },
        locs: [...byFile].filter(([, locs]) => locs.length).map(([file, locs]) => ({ file, locs })),
    };
}

/** LSP documentation (string | MarkupContent) as text. */
function docText(/** @type {any} */ d) {
    return typeof d === 'string' ? d : d?.value || '';
}

/** `textDocument/signatureHelp` to a `signatureHelp` body (undefined: no help). */
function signatureHelpBody(/** @type {any} */ help, /** @type {TsPos} */ at) {
    if (!help || !Array.isArray(help.signatures) || help.signatures.length === 0) return undefined;
    const items = help.signatures.map((/** @type {any} */ sig) => {
        const label = String(sig.label);
        /** @type {[number, number][]} */
        const ranges = [];
        let from = 0;
        for (const p of sig.parameters || []) {
            if (Array.isArray(p.label)) ranges.push([p.label[0], p.label[1]]);
            else {
                const i = label.indexOf(String(p.label), from);
                const s = i < 0 ? from : i;
                ranges.push([s, s + String(p.label).length]);
            }
            from = ranges[ranges.length - 1][1];
        }
        let prefix = label;
        let suffix = '';
        let separator = ', ';
        if (ranges.length > 0) {
            prefix = label.slice(0, ranges[0][0]);
            suffix = label.slice(ranges[ranges.length - 1][1]);
            if (ranges.length > 1) separator = label.slice(ranges[0][1], ranges[1][0]);
        } else {
            const open = label.indexOf('(');
            if (open >= 0) {
                prefix = label.slice(0, open + 1);
                suffix = label.slice(open + 1);
            }
        }
        const doc = docText(sig.documentation);
        return {
            isVariadic: false,
            prefixDisplayParts: [{ text: prefix, kind: 'text' }],
            suffixDisplayParts: [{ text: suffix, kind: 'text' }],
            separatorDisplayParts: [{ text: separator, kind: 'punctuation' }],
            parameters: ranges.map(([s, e], i) => {
                const name = label.slice(s, e);
                const pdoc = docText((sig.parameters || [])[i]?.documentation);
                return {
                    name: name.replace(/[?:].*$/s, '').replace(/^\.\.\./, ''),
                    documentation: pdoc ? [{ text: pdoc, kind: 'text' }] : [],
                    displayParts: [{ text: name, kind: 'text' }],
                    isOptional: /^[^:]*\?/.test(name),
                    isRest: name.startsWith('...'),
                };
            }),
            documentation: doc ? [{ text: doc, kind: 'text' }] : [],
            tags: [],
        };
    });
    const active = help.signatures[help.activeSignature ?? 0] || help.signatures[0];
    const argumentIndex = active.activeParameter ?? help.activeParameter ?? 0;
    return {
        items,
        applicableSpan: { start: at, end: at },
        selectedItemIndex: help.activeSignature ?? 0,
        argumentIndex,
        argumentCount: Math.max(argumentIndex + 1, (active.parameters || []).length),
    };
}

/** VS Code's semantic token legend for TypeScript (tsserver's 2020 encoding). */
const TS_TOKEN_TYPES = ['class', 'enum', 'interface', 'namespace', 'typeParameter', 'type', 'parameter', 'variable', 'enumMember', 'property', 'function', 'method'];
const TS_TOKEN_MODIFIERS = ['declaration', 'static', 'async', 'readonly', 'defaultLibrary', 'local'];

/**
 * LSP semantic tokens to tsserver `encodedSemanticClassifications-full` spans
 * (`[start, length, ((type + 1) << 8) | modifiers]`, offsets in the file's text).
 * @param {any} tokens `{data: number[]}`
 * @param {{tokenTypes: string[], tokenModifiers: string[]}} legend the server's legend
 * @param {TextDoc} doc
 * @param {number} start
 * @param {number} length
 */
function encodedClassifications(tokens, legend, doc, start, length) {
    const data = tokens?.data || [];
    const typeMap = legend.tokenTypes.map((t) => TS_TOKEN_TYPES.indexOf(t));
    const modMap = legend.tokenModifiers.map((m) => TS_TOKEN_MODIFIERS.indexOf(m));
    /** @type {number[]} */
    const spans = [];
    let line = 0;
    let ch = 0;
    for (let i = 0; i + 4 < data.length; i += 5) {
        line += data[i];
        ch = data[i] === 0 ? ch + data[i + 1] : data[i + 1];
        const type = typeMap[data[i + 3]];
        if (type === undefined || type < 0) continue;
        let mods = 0;
        for (let b = 0; b < modMap.length; b++) {
            if (data[i + 4] & (1 << b) && modMap[b] >= 0) mods |= 1 << modMap[b];
        }
        const offset = doc.indexOf(line, ch);
        if (offset + data[i + 2] < start || offset > start + length) continue;
        spans.push(offset, data[i + 2], ((type + 1) << 8) | mods);
    }
    return { spans, endOfLineState: 0 };
}

/**
 * `textDocument/documentSymbol` to a `navtree` body (VS Code's outline, breadcrumbs and
 * sticky scroll read the root's children).
 * @param {any[] | null} symbols DocumentSymbol[] or SymbolInformation[]
 * @param {TextDoc} doc
 */
function navTree(symbols, doc) {
    /** @returns {any} */
    const convert = (/** @type {any} */ s) => {
        const range = s.range || s.location?.range;
        /** @type {any} */
        const item = {
            text: s.name,
            kind: SYMBOL_KIND[s.kind] || 'var',
            kindModifiers: '',
            spans: [spanOf(range)],
            childItems: (s.children || []).map(convert),
        };
        if (s.selectionRange) item.nameSpan = spanOf(s.selectionRange);
        return item;
    };
    return {
        text: '<global>',
        kind: 'script',
        kindModifiers: '',
        spans: [{ start: { line: 1, offset: 1 }, end: doc.endPos() }],
        childItems: (symbols || []).map(convert),
    };
}

/**
 * An LSP `WorkspaceEdit` as tsserver `FileCodeEdits[]` (`changes` or `documentChanges`; a
 * created file is `isNewFile`, its text the edit at its start).
 * @param {any} edit
 * @param {(file: string) => string} [rename] maps file names (old paths to new after a move)
 */
function fileCodeEdits(edit, rename = (f) => f) {
    /** @type {Map<string, any>} */
    const byFile = new Map();
    /** @param {string} uri */
    const entry = (uri) => {
        const fileName = rename(uriToPath(uri));
        let e = byFile.get(fileName);
        if (!e) {
            e = { fileName, textChanges: [] };
            byFile.set(fileName, e);
        }
        return e;
    };
    /** @param {string} uri @param {any[]} edits */
    const add = (uri, edits) => {
        const e = entry(uri);
        for (const t of edits || []) e.textChanges.push({ ...spanOf(t.range), newText: t.newText });
    };
    for (const [uri, edits] of Object.entries(edit?.changes || {})) add(uri, /** @type {any[]} */ (edits));
    for (const dc of edit?.documentChanges || []) {
        if (dc.kind === 'create') entry(dc.uri).isNewFile = true;
        else if (dc.textDocument) add(dc.textDocument.uri, dc.edits);
    }
    return [...byFile.values()].filter((e) => e.textChanges.length || e.isNewFile);
}

/** tsserver's `fixName` for ts-r's fix ids (as tsserver 6.0.3 reports them). @type {Record<string, string>} */
const FIX_NAMES = {
    fixMissingImport: 'import',
    fixSpelling: 'spelling',
    unusedIdentifier_delete: 'unusedIdentifier',
    unusedIdentifier_prefix: 'unusedIdentifier',
};

/**
 * Quick fixes (LSP code actions of kind `quickfix`) as a `getCodeFixes` body. Fix-all actions
 * become the `fixAllDescription` of the fixes with their `fixId`.
 * @param {any[] | null} actions
 */
function codeFixesBody(actions) {
    const list = actions || [];
    /** @type {Map<string, string>} */
    const fixAll = new Map();
    for (const a of list) if (a.data?.fixAll && a.data.fixId) fixAll.set(a.data.fixId, a.title);
    return list
        .filter((a) => !a.data?.fixAll && a.edit && (!a.kind || a.kind === 'quickfix'))
        .map((a) => {
            /** @type {any} */
            const id0 = a.data?.fixId;
            const fixName = id0 === 'addMissingAwait' && /to initializer/.test(a.title) ? 'addMissingAwaitToInitializer' : FIX_NAMES[id0] || id0 || 'ts-r';
            /** @type {any} */
            const fix = { fixName, description: a.title, changes: fileCodeEdits(a.edit) };
            const id = a.data?.fixId;
            if (id && fixAll.has(id)) {
                fix.fixId = id;
                fix.fixAllDescription = fixAll.get(id);
            }
            return fix;
        });
}

/**
 * Refactors (LSP code actions with `data.refactor` = `name/action`) as a
 * `getApplicableRefactors` body.
 * @param {any[] | null} actions
 */
function refactorsBody(actions) {
    /** @type {Map<string, any>} */
    const byName = new Map();
    for (const a of actions || []) {
        const r = a.data?.refactor;
        if (typeof r !== 'string') continue;
        const i = r.indexOf('/');
        if (i < 0) continue;
        const name = r.slice(0, i);
        let info = byName.get(name);
        if (!info) {
            info = { name, description: name, actions: [] };
            byName.set(name, info);
        }
        info.actions.push({ name: r.slice(i + 1), description: a.title, kind: a.kind });
    }
    return [...byName.values()];
}

/**
 * One of `ts-r/projectInfo`'s `configDiagnostics` as a `configFileDiag` entry. VS Code shows
 * them on the config file; a range only when it points into that file.
 * @param {{code: number, message: string, severity: number, file?: string | null, range?: {start: LspPos, end: LspPos} | null}} d
 * @param {string} configFile
 */
function configDiagnostic(d, configFile) {
    /** @type {any} */
    const out = { text: d.message, code: d.code, category: d.severity === 1 ? 'error' : 'warning' };
    if (d.range && d.file === configFile) {
        out.start = toTs(d.range.start);
        out.end = toTs(d.range.end);
    }
    return out;
}

module.exports = {
    TextDoc,
    toLsp,
    toTs,
    pathToUri,
    uriToPath,
    languageId,
    splitDiagnostics,
    quickInfo,
    completionInfo,
    entryDetails,
    fileSpans,
    referencesBody,
    highlightsBody,
    renameBody,
    RENAME_PLACEHOLDER,
    signatureHelpBody,
    encodedClassifications,
    navTree,
    configDiagnostic,
    fileCodeEdits,
    codeFixesBody,
    refactorsBody,
};
