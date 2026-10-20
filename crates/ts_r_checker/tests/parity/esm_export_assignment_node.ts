// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "nodenext", "noEmit": true}, "files": ["a.ts", "b.d.ts", "c.d.mts", "d.mts", "e.cts", "sub/h.ts", "sub/i.d.ts"]}
// @filename: package.json
{"name": "p", "type": "module"}
// @filename: sub/package.json
{"type": "commonjs"}
// @filename: a.ts
const a = 1;
export = a;
// @filename: b.d.ts
declare const b: number;
export = b;
// @filename: c.d.mts
declare const c: number;
export = c;
// @filename: d.mts
const d = 1;
export = d;
// @filename: e.cts
const e = 1;
export = e;
// @filename: sub/h.ts
const h = 1;
export = h;
// @filename: sub/i.d.ts
declare const i: number;
export = i;
