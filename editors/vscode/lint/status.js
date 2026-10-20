// @ts-check
// The ts-r checker's state (`ts-r/status`, K-17 parity check) from the lint connection as status
// bar text and a tooltip (the tsserver bridge's engines are checker.js). No VS Code dependency,
// so `npm test` covers it.
'use strict';

/**
 * @typedef {{project: string, state: string, openFiles?: number,
 *   parity?: {result: string, detail?: string, typescript?: string, ours?: number, theirs?: number,
 *     onlyOurs?: string[], onlyTheirs?: string[], report?: string | null, cached?: boolean} | null}} ProjectStatus
 * @typedef {{backend?: string, version?: string, summary?: string, fallbackTypeScript?: string | null,
 *   projects?: ProjectStatus[]}} TsrStatus
 */

/**
 * Status bar text and tooltip for a `ts-r/status` payload (null: nothing to show).
 * @param {TsrStatus | null | undefined} st
 * @returns {{text: string, tooltip: string, warning: boolean} | null}
 */
function describeStatus(st) {
    if (!st || st.backend !== 'ts-r') return null;
    const projects = st.projects || [];
    const fallback = projects.filter((p) => p.state === 'fallback');
    const checking = projects.filter((p) => p.state === 'checking');
    let text = '$(check) ts-r';
    if (fallback.length > 0) text = `$(warning) ts-r: ${fallback.length} on TypeScript`;
    else if (checking.length > 0) text = '$(sync~spin) ts-r: parity check';
    const lines = [st.summary || 'ts-r checker'];
    for (const p of projects) {
        const detail = p.parity?.detail ? ` (${p.parity.detail})` : '';
        lines.push(`${p.project}: ${p.state}${detail}`);
    }
    if (fallback.length > 0) lines.push('Click to see and report the difference.');
    return { text, tooltip: lines.join('\n'), warning: fallback.length > 0 };
}

module.exports = { describeStatus };
