// `S[K]` relates to `T[J]` when `S` relates to `T` and `K` to `J`, or through the target's
// write constraint; the error elaborates the object or key comparison. A string index
// signature answers symbol keys in a base constraint (`Record<string, any>[keyof T]`).
function f<T, U extends T, K extends keyof T, J extends K>(t: T, u: U, k: K, j: J) {
    let a: T[K] = u[k];
    let b: U[K] = t[k];
    let c: T[J] = t[k];
    let d: T[K] = t[j];
    let e: U[J] = t[k];
}
function test1<T extends Record<string, any>, K extends keyof T>(t: T, k: K) {
    t[k] = 42;
}
function f12<T extends Record<string, number>, K extends keyof T>(obj: T, k3: K) {
    obj[k3] = 123;
}
function fn2<T extends Array<string>>(param: T, cb: (element: T[number]) => void) {
    cb(param[0]!);
}
