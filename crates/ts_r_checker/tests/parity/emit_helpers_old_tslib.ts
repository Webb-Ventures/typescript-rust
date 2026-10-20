// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2016", "lib": ["esnext"], "importHelpers": true, "module": "commonjs", "noEmit": true}, "files": ["a.ts", "b.ts", "c.ts"]}
// @filename: a.ts
// A tslib without some helpers: TS2343 per helper and file, at its first use.
export {};
async function foo(): Promise<void> {}
async function bar(): Promise<void> {}
export function spread({ a, ...rest }: { a: number; b: number }) {
    return { c: "c", ...rest };
}
// @filename: b.ts
export {};
async function baz(): Promise<void> {}
// @filename: c.ts
// Pre-4.3 private field helpers take too few parameters (TS2807).
export class C {
    #a = 1;
    #b() { this.#c = 42; }
    set #c(v: number) { this.#a += v; }
}
// @filename: node_modules/tslib/package.json
{ "name": "tslib", "main": "tslib.js", "typings": "tslib.d.ts" }
// @filename: node_modules/tslib/tslib.d.ts
export declare function __classPrivateFieldGet<T extends object, V>(receiver: T, state: any): V;
export declare function __classPrivateFieldSet<T extends object, V>(receiver: T, state: any, value: V): V;
export declare const notAHelper: any;
