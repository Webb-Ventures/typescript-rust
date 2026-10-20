// @ts-check
// `ts-r.lint.enable`: whether ts-r-lint runs in this window. Pure logic, tested without VS Code.
'use strict';

/** The ESLint extension's id. */
const ESLINT_EXTENSION = 'dbaeumer.vscode-eslint';

/**
 * @typedef {'auto' | 'on' | 'off'} LintEnable
 * @typedef {{ installed: boolean, eslintEnable: boolean }} EslintState
 *   `installed`: VS Code lists the ESLint extension (disabled extensions are not listed);
 *   `eslintEnable`: its `eslint.enable` setting for this workspace (default true).
 */

/**
 * Decides whether ts-r lints. `on` and `off` are the user's explicit choice. `auto` (the
 * default) lints unless the ESLint extension is active in this workspace, so nobody gets the
 * same problem twice; set `on` to opt in (and turn ESLint off for the workspace).
 * @param {unknown} setting the `ts-r.lint.enable` value
 * @param {EslintState} eslint
 * @returns {{ lint: boolean, reason: string }}
 */
function resolveLintEnable(setting, eslint) {
    const value = setting === 'on' || setting === 'off' ? setting : 'auto';
    if (value === 'on') return { lint: true, reason: 'ts-r.lint.enable is "on"' };
    if (value === 'off') return { lint: false, reason: 'ts-r.lint.enable is "off"' };
    if (eslint.installed && eslint.eslintEnable) {
        return {
            lint: false,
            reason: `the ESLint extension (${ESLINT_EXTENSION}) is enabled in this workspace; set ts-r.lint.enable to "on" to use ts-r-lint instead`,
        };
    }
    return { lint: true, reason: eslint.installed ? 'eslint.enable is false for this workspace' : 'the ESLint extension is not enabled' };
}

module.exports = { resolveLintEnable, ESLINT_EXTENSION };
