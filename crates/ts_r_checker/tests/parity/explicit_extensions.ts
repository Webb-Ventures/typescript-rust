// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "nodenext", "noEmit": true, "allowJs": true}, "files": ["a.mts", "p.ts", "p.cts", "q.ts", "q.d.ts", "r.d.ts", "r.js", "s.cts", "s.mts", "t.js", "t.ts", "u.d.mts", "u.d.ts"]}
// @filename: a.mts
import "./p";
import "./q";
import "./r";
import "./s";
import "./t";
import "./u";
import * as p from "./p";
import * as q from "./q";
import * as r from "./r";
import * as s from "./s";
import * as t from "./t";
import * as u from "./u";
export { p, q, r, s, t, u };
// @filename: p.ts
export {};
// @filename: p.cts
export {};
// @filename: q.ts
export {};
// @filename: q.d.ts
export {};
// @filename: r.d.ts
export {};
// @filename: r.js
export {};
// @filename: s.cts
export {};
// @filename: s.mts
export {};
// @filename: t.js
export {};
// @filename: t.ts
export {};
// @filename: u.d.mts
export {};
// @filename: u.d.ts
export {};
