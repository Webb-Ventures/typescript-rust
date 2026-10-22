// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"target":"es2022","noEmit":true,"lib":["esnext"]},"files":["a.ts"]}
// @filename: a.ts
declare function gen(): Generator<number, string, unknown>;
declare function agen(): AsyncGenerator<number, boolean, unknown>;
declare const it: { [Symbol.iterator](): { next(): { done: false; value: 1 } | { done: true; value: "r" } } };
declare const it2: { [Symbol.iterator](): { next(): { done: boolean; value: 1 } } };
declare const inferred: <T, U, V>() => Generator<T, U, V>;
function* g1() {
    const a: number = yield* gen();
    const b: number = yield* "abc";
    const c: number = yield* it;
    const d: number = yield* it2;
    const e: number = yield* new Set<string>();
}
async function* g2() {
    const a: number = yield* agen();
    const b: number = yield* [1, 2];
    const c: number = yield* gen();
}
function* g3(): Generator<number, void, unknown> {
    const a: number = yield* gen();
    const b: number = yield* inferred();
    const c: string = yield* inferred<number, number, unknown>();
}
function* g4() {
    var o = []
    o = yield* o
}
