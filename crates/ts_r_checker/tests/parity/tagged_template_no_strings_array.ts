// Without a global `TemplateStringsArray` a tag's first argument is `{}` (global TS2318):
// `{}` lacks `raw`, is not a `number`, and infers `T` as `{}`.
declare function f(s: { raw: string[] }, a: number): void;
f`${"s"}`;
declare function g(s: number): void;
g`x`;
declare function h<T>(s: T): T;
const r = h`x`;
const n: number = r;
// @filename: lib.d.ts
interface Array<T> { length: number; [n: number]: T }
interface ReadonlyArray<T> { readonly length: number; readonly [n: number]: T }
interface Boolean {}
interface Function {}
interface CallableFunction {}
interface NewableFunction {}
interface IArguments {}
interface Number {}
interface Object {}
interface RegExp {}
interface String {}
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "noLib": true, "target": "ES2023", "noEmit": true}, "files": ["a.ts", "lib.d.ts"]}
