// A generic template literal target matches by pattern or through the source's
// constraint; `${T}` is no `T`. A union against a type parameter elaborates its first
// failing member, unless the hint is "could be instantiated with an arbitrary type".
function f<T extends "a" | "b">(x: T, u: "a" | "b", z: `${T}`) {
    const a: `${T}` = x;
    const b: T = z;
    const c: T = u;
    const d: `${T}` = z;
    const e: `x${T}` = `x${z}`;
}
function g<T, Top, U>(x: T | Top | U) {
    const y: Top = x;
}
type Num<T extends number | `${number}`> = T;
type Keys<T extends unknown[]> = { [K in keyof T]: Num<K> };
