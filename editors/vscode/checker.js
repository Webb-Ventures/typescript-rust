// @ts-check
// `ts-r.checker` in the editor: status bar text for the tsserver bridge's engines
// (`_ts-r.engines`, answered by standin/router.js) and the prefilled GitHub issue of
// "ts-r: Report Parity Mismatch". No VS Code dependency, so `npm test` covers it.
'use strict';

const ISSUES = 'https://github.com/Webb-Ventures/typescript-rust/issues/new';

/**
 * @typedef {{project: string, config: string | null, state: string, engine: 'ts-r' | 'typescript', openFiles: number}} EngineProject
 * @typedef {{bridge: true, checker: string, tsrAvailable: boolean, projects: EngineProject[], files?: Record<string, string>,
 *   typescript: {running: boolean, starts: number}, syntax: {running: boolean}, status: any}} Engines
 */

/** Why a project is on the engine it is on. @param {EngineProject} p @param {string} checker @param {any} parity */
function reason(p, checker, parity) {
    if (p.engine === 'ts-r') return checker === 'on' ? 'ts-r (ts-r.checker: on)' : 'ts-r (parity check passed)';
    switch (p.state) {
        case 'fallback':
            return `TypeScript (parity difference${parity?.detail ? `: ${parity.detail}` : ''})`;
        case 'checking':
            return 'TypeScript (parity check running)';
        case 'unverified':
        case 'unknown':
            return `TypeScript (parity not verified${parity?.detail ? `: ${parity.detail}` : ''})`;
        default:
            return 'TypeScript';
    }
}

/**
 * Status bar text, tooltip and warning flag for the bridge's engines. `active` is the active
 * editor's file: the text names the engine serving it.
 * @param {Engines | null | undefined} e
 * @param {string | undefined} active
 * @returns {{text: string, tooltip: string, warning: boolean} | null}
 */
function describeEngines(e, active) {
    if (!e || !e.bridge) return null;
    const projects = e.projects || [];
    const parity = new Map((e.status?.projects || []).map((/** @type {any} */ p) => [p.config || p.project, p.parity]));
    const onTsr = projects.filter((p) => p.engine === 'ts-r');
    const onTs = projects.filter((p) => p.engine !== 'ts-r');
    const mismatch = projects.some((p) => p.state === 'fallback');
    const checking = projects.some((p) => p.state === 'checking');
    const activeEngine = active && e.files ? e.files[active] : undefined;
    let text;
    if (!e.tsrAvailable) text = '$(warning) ts-r: TypeScript';
    else if (activeEngine === 'ts-r') text = onTs.length ? `$(check) ts-r · ${onTs.length} on TypeScript` : '$(check) ts-r';
    else if (activeEngine === 'typescript') text = checking ? '$(sync~spin) TypeScript (ts-r parity check)' : '$(server-process) TypeScript';
    else if (projects.length === 0) text = '$(check) ts-r';
    else text = onTsr.length ? `$(check) ts-r ${onTsr.length} · TypeScript ${onTs.length}` : '$(server-process) TypeScript';
    const lines = [`ts-r.checker: ${e.checker}`];
    if (!e.tsrAvailable) lines.push('The ts-r checker is not available (see the TypeScript server log); every project uses TypeScript.');
    for (const p of projects) lines.push(`${p.project}: ${reason(p, e.checker, parity.get(p.config || p.project))}`);
    lines.push(
        e.typescript?.running
            ? 'TypeScript server: running (projects above, or on demand for features ts-r does not have yet)'
            : 'TypeScript server: not running',
    );
    if (mismatch) lines.push('Click to report a parity mismatch or change ts-r.checker.');
    else lines.push('Click for details or to change ts-r.checker.');
    return { text, tooltip: lines.join('\n'), warning: mismatch && activeEngine !== 'ts-r' };
}

/** `TS2322` from a parity key (`file(line,col): TS2322` or `TS2322`). @param {string} key */
function codeOf(key) {
    return /TS(\d+)\s*$/.exec(key)?.[1];
}

/** @param {string[]} keys @returns {[string, number][]} codes by count, then code */
function countCodes(keys) {
    /** @type {Map<string, number>} */
    const m = new Map();
    for (const k of keys || []) {
        const c = codeOf(k);
        if (c) m.set(c, (m.get(c) || 0) + 1);
    }
    return [...m].sort((a, b) => b[1] - a[1] || Number(a[0]) - Number(b[0]));
}

/**
 * The GitHub issue for parity mismatches: only versions and, per project, diagnostic codes with
 * counts. Never file names, positions, messages or source text.
 * @param {any} status `ts-r/status`
 * @param {{editor?: string, platform?: string}} env
 * @returns {{title: string, body: string, url: string} | null} null: nothing to report
 */
function parityIssue(status, env) {
    const projects = (status?.projects || []).filter((/** @type {any} */ p) => p.parity?.result === 'mismatch');
    if (projects.length === 0) return null;
    const first = projects[0].parity;
    const lines = [
        'Parity mismatch reported from the ts-r VS Code extension (diagnostic codes and counts only).',
        '',
        `- ts-r: ${status.version || '?'}`,
        `- TypeScript: ${first.typescript || '?'}`,
        `- Editor: ${env.editor || '?'}; platform: ${env.platform || '?'}`,
        `- Projects checked: ${(status.projects || []).length}; mismatched: ${projects.length}`,
    ];
    projects.forEach((/** @type {any} */ p, /** @type {number} */ i) => {
        const r = p.parity;
        lines.push('', `### Project ${i + 1}`, `ts-r: ${r.ours ?? '?'} diagnostics, tsc: ${r.theirs ?? '?'}`);
        const fmt = (/** @type {[string, number][]} */ list) => list.slice(0, 25).map(([c, n]) => `TS${c} ×${n}`).join(', ') || 'none';
        lines.push(`- only tsc reports (among the first listed differences): ${fmt(countCodes(r.onlyTheirs))}`);
        lines.push(`- only ts-r reports (among the first listed differences): ${fmt(countCodes(r.onlyOurs))}`);
    });
    lines.push('', '<!-- Add a minimal snippet that reproduces one of these codes if you can share one. -->');
    const title = `Parity mismatch: ${countCodes(projects.flatMap((/** @type {any} */ p) => [...(p.parity.onlyTheirs || []), ...(p.parity.onlyOurs || [])]))
        .slice(0, 3)
        .map(([c]) => `TS${c}`)
        .join(', ')}`;
    const body = lines.join('\n');
    const url = `${ISSUES}?labels=parity&title=${encodeURIComponent(title)}&body=${encodeURIComponent(body)}`;
    return { title, body, url };
}

module.exports = { describeEngines, parityIssue, countCodes, reason };
