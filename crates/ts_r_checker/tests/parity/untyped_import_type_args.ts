// Type arguments of an import type whose module has no types are not looked at (TS7016 once).
// @filename: node_modules/untyped/package.json
{ "name": "untyped", "version": "1.0.0", "main": "index.js" }
// @filename: node_modules/untyped/index.js
module.exports = {};
// @filename: a.d.ts
export declare const a: import("untyped").A<import("untyped").B>;
export declare const b: import("untyped").A | import("untyped").B;
