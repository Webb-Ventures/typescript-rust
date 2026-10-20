// `const X = Symbol()` declares a unique symbol (`typeof X`).
const A = Symbol("A");
const B = Symbol();
const n: number = A;
let widened = A;
const w: number = widened;

// Computed unique-symbol keys are contextually typed by the matching property or the
// symbol index signature.
const viaIndex: { [sym: symbol]: (p: string) => void } = { [A]: s => s.length };
const viaProp: { [A]: (p: string) => void } = { [A]: s => s.length };
type Action = { type: typeof A; data: string } | { type: typeof B; data: number };
declare const ab: Action;
declare function on<T extends { type: string | symbol }>(action: T, handlers: { [K in T["type"]]: (p: K) => void }): void;
on(ab, {
    [A]: a => a.description,
    [B]: b => b.description,
});

// Symbol-keyed property names print as `[A]` (`[Symbol.iterator]` for well-known ones).
interface HasA {
    [A]: boolean;
}
const missing: HasA = {};

// `sym in x` narrows like a literal key.
function narrow(x: object) {
    if ("a" in x && A in x) {
        x.a;
        x[A];
    }
}

// Late-bound expando properties: `f[sym] = v` declares `f[sym]`.
export function f() {}
f.bar = 12;
f[B] = "ok";
const fromExpando: string = f[B];
const wrong: number = f[B];

// Generators take yield and return contexts from a union return type, member by member.
type FuncOrGen = () => number | Generator<(arg: number) => void, (arg: string) => void, void>;
const g: FuncOrGen = function* () {
    yield (num) => num.toFixed();
    return (s) => s.length;
};
function* annotated(): Generator<(x: number) => string> {
    yield (x) => x.toFixed();
    yield (x) => x;
}

export { n, w, viaIndex, viaProp, missing, narrow, fromExpando, wrong, g, annotated };
