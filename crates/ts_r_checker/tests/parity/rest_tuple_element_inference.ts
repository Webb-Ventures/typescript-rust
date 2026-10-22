// A rest element of a tuple target infers once from the union of the source's middle
// element types, so `U` is `string | boolean` rather than failing between candidates.
declare function f0<T, U>(x: [T, ...U[]]): [T, U];
const r = f0([1, "hello", true]);
const n: number = r;
