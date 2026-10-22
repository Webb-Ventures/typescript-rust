// A type alias that names itself through a generic interface type argument links the
// reference once known; an optional property of it adds no `| undefined` to its name.
interface Box<T> { value: T }
type A = undefined | number | Box<A>;
type B = undefined | number | B[];
type C = undefined | number | { x: C };
type N = string | Iterable<N>;
interface P { a?: A; b?: B; c?: C }
const p: P = { a: "x" };
const q: P = { b: "x" };
const r: P = { c: "x" };
const z: A = { a: 1 };
const f = () => "s";
const n: N = f;
const ok1: A = { value: { value: 1 } };
const ok2: N = ["a", ["b"]];
export {};
