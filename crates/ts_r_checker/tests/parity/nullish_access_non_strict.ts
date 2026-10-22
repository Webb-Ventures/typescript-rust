// @filename: tsconfig.json
{"compilerOptions": {"strict": false, "target": "ES2023", "module": "ESNext", "noEmit": true}, "files": ["a.ts"]}
// @filename: a.ts
// Without strictNullChecks a value that is only `undefined` or `null` is still reported
// when accessed; `++undefined` is TS2539 alone.
declare const u: undefined;
u.x;
declare const n: null;
n.x;
this.x;
const f = () => this.y;
undefined.x;
null.x;
(null).x;
++undefined;
export {};
