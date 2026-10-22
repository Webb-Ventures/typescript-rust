// `T[keyof T]` whose `T` has no index signature relates as `T[string] | T[number] |
// T[symbol]`; `T[K]` with `K extends keyof T` as `T[keyof T]`. Both are `unknown` to
// `{} | null | undefined`.
function f1<T extends object>(a: T[keyof T]) {
    let b: number = a;
}
function f2<T>(a: T[keyof T]) {
    let b: number = a;
}
function f3<T extends object, K extends keyof T>(a: T[K]) {
    let b: number = a;
}
function f4<T extends Record<string, number>>(a: T[keyof T]) {
    let b: number = a;
}
function f5<T, K extends keyof T>(x: T, y: T[keyof T], z: T[K]) {
    let a: {} | null | undefined;
    a = x;
    a = y;
    a = z;
}
