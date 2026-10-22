// The right operand of `in` must be assignable to `object` (TS2322); the `{}` of a
// narrowed `unknown` may be a primitive (TS2638). A written `{}` and `never` are fine.
function f1(x: {}, y: unknown, n: number, so: string | object, v: never, a: any) {
    const a1 = "foo" in x;
    if (y) {
        const a2 = "foo" in y;
    }
    const a3 = "foo" in n;
    const a4 = "foo" in so;
    const a5 = "foo" in v;
    const a6 = "foo" in a;
}
function f2<T>(obj: T, key: keyof T) {
    const a = "foo" in obj;
    const b = "foo" in obj[key];
}
function f3<T extends {}>(obj: T) {
    const a = "foo" in obj;
}
function f4<T extends string | object>(obj: T) {
    const a = "foo" in obj;
}
function f5(x: {} | undefined, e: { a: 1 } | number) {
    if (x) {
        const a = "foo" in x;
    }
    const b = "foo" in e;
}
