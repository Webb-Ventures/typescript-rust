// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "esnext", "moduleResolution": "bundler", "noEmit": true, "allowJs": true, "checkJs": true}, "files": ["a.ts", "b.d.ts", "c.d.mts", "d.mts", "e.cts", "g.ts"]}
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
// @filename: g.ts
declare module "m" {
    const g: number;
    export = g;
}
export {};
