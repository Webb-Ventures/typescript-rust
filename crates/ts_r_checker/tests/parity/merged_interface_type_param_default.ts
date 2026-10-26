// @filename: a.d.ts
declare namespace N {
    interface I<T, U> { a: T; b: U }
    interface J<T, U> { a: T; b: U }
}
// @filename: b.d.ts
declare namespace N {
    interface I<T, U = any> { a: T; b: U }
    interface J<T, U = T[]> { a: T }
}
// @filename: main.ts
export let x: N.I<string>;
export let y: N.J<string>;
export const z: number = y.b;
