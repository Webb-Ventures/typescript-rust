type Model = { a: string; b: number };
type MM<S extends string> = { [K in keyof Model as `${K}${S}`]: Model[K] };
type Box<T> = { [P in keyof T]: { v: T[P] } };

function e1<T, U extends { a: string }, K extends string, S extends string, J extends keyof U>() {
    const a1: Partial<T> = { a: 1 };
    const a2: Partial<U> = { a: "x" };
    const a3: Partial<U> = { a: "x", z: 1 };
    const a4: Record<K, number> = { a: 1 };
    const a5: MM<S> = { a: "x" };
    const a6: Readonly<U> = { a: "x", z: 1 };
    const a7: Pick<U, J> = { a: "x" };
    const a8: Partial<Record<K, number>> = { a: 1 };
    const a9: Box<U> = { a: { v: "x" }, z: 1 };
    const b1: Partial<T> & { x: number } = { x: 1, y: 2 };
    const b2: Partial<T> | { x: number } = { x: 1, y: 2 };
    const b3: { [P in K | "c"]: number } = { c: 1, d: 2 };
    const b4: Omit<U, "a"> = { a: "x" };
    const b5: Required<U> = { a: "x", z: 1 };
}
