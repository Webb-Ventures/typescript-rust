// @filename: b.mts
// In an ES module a CommonJS file's default (as a default or `{ default }` import, or a
// re-export) is the whole module under node16..nodenext.
import a from "./mod.cjs";
import { default as b } from "./mod.cjs";
import * as self from "./b.mjs";
export { default } from "./mod.cjs";
a();
b();
self.default();
a.default();
self.default.default();
// @filename: mod.cts
declare function fun(): void;
export default fun;
// @filename: a.cts
import m = require("./mod.cjs");
export = m;
// @filename: c.cts
// A `{ default }` import of a TypeScript file with `export =` is the `export =` entity.
import d, { default as e } from "./a.cjs";
d();
e();
e.default();
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2022", "lib": ["ES2023"], "module": "node16", "noEmit": true }, "files": ["b.mts", "mod.cts", "a.cts", "c.cts"] }
