// Unannotated generators infer `Generator<Y, R, N>`: Y from the yields, R from the returns,
// N from what resumes them (`yield*` operands, contextually typed yield results), else the
// context's, else `unknown`. A used yield result without a type is TS7057.
export {};
function* g0() { yield 0; }
const q0: number = g0;
const o1 = { *m(_: number) { yield 0; } };
const q1: number = o1;
async function* ag() { yield 1; return "s"; }
const q2: number = ag;
const a1: () => Iterable<number> = function* () { yield "a"; };
const a2: () => Iterable<number> = function* () { yield* ["a"]; };
const a5: () => Iterator<number> = function* () { yield "a"; };
const a6: () => Iterable<number, any, string> = function* () { yield "a"; };
const a8: () => Iterable<number> = function* () { const x: number = yield "a"; };
function* d1() { const x: number = yield 1; }
const q3: string = d1;
function* d2() { const x: number = yield 1; const y: string = yield 2; }
const q4: string = d2;
function* d3() { yield* d1(); }
const q5: string = d3;
function* d4() { const x = yield 1; }
function* d5() {
    yield;
    (yield);
    void (yield);
    const [p = 1] = yield;
}
function* d6() { return 1; }
const q6: string = d6;
declare const ii: IterableIterator<number>;
const q7: string = ii;
declare const ai: AsyncIterable<number, void>;
const q8: string = ai;
declare const it: Iterator<number>;
const q9: string = it;
