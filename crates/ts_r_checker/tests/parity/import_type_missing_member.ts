// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler"}, "files": ["a.ts", "b.ts", "c.js", "d.js", "dotted.js", "view.jsx"]}
// @filename: b.ts
export const FOO = "foo";
export type T = 1;
export namespace N { export const v = 1; export type W = 2; }
// @filename: a.ts
// `import("m").X` names a type: a missing or value-only member is TS2694.
type A = import("./b").FOO;
type B = import("./b").NOPE;
type C = import("./b").N.v;
type D = import("./b").N.W;
type E = import("./b").T;
type F = import("./dotted").Outer.Inner;
import View from "./view";
export type { A, B, C, D, E, F };
export { View };
// @filename: c.js
export const FOO = "foo";
// @filename: d.js
/** @type {import("./c").FOO} */
export let x;
/** @type {import("./c").NOPE} */
export let y;
// @filename: dotted.js
/** @typedef {number} Outer.Inner */
export const Outer = {};
// @filename: view.jsx
// A `.jsx` module without `jsx` set is TS6142 where it is imported.
export default function View() { return null; }
