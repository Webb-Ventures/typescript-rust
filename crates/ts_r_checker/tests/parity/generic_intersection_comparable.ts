// A generic intersection is compared with a concrete type as its constraint.
function f5<T extends string | number>(x: T & number) {
    const t1 = x === "hello";
    const t2 = x === 1;
}
function f6<T extends { a: string }>(x: T & { b: number }) {
    const t1 = x === { a: "", b: 1 };
    const t2 = x === "s";
}
