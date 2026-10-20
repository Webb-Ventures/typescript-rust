// @filename: tsconfig.json
{"compilerOptions": {"allowJs": true, "noEmit": true, "target": "ES2022"}, "files": ["a.js", "b.js"]}
// @filename: a.js
// A `@typedef {T}` without a name is a syntax error one character before where the name
// should be, even in a file that is not checked: at the closing brace, before a following
// tag, or where the type stopped parsing.
/**
 * @typedef {string}
 */
var x;

/** @typedef {{ a: number }} */
foo.bar;

/**
 * @typedef {Object}
 * @property {string} p
 */
function f() {}
// @filename: b.js
/** @typedef */
var y;
/** @typedef {} */
var z;
/** @typedef {number */
var w;
/** @typedef {function(string): boolean} */
var v;
/** @typedef {number}
 * Named */
var u;
