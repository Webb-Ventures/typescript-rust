// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "nodenext", "noEmit": true}, "files": ["a.mts", "b.cts", "c.ts"]}
// @filename: a.mts
export const a = <T>() => 1;
export const b = <T,>() => 1;
export const c = <T extends unknown>() => 1;
export const d = <T, U>(x: T, y: U) => [x, y];
export const e = async <T>(x: T) => x;
export const f = <T /* comment */,>(x: T) => x;
export const g = <T // comment
,>(x: T) => x;
export const h = <T /* first */ /* second */, /* last */>(x: T) => x;
// @filename: b.cts
export const a = <T>(x: T) => x;
export const b = <T /* comment */,>(x: T) => x;
export const c = <T // comment
,>(x: T) => x;
// @filename: c.ts
export const a = <T>() => 1;
