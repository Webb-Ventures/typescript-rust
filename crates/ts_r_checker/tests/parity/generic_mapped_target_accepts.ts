type Box<T> = { [P in keyof T]: { v: T[P] } };
type Getters<T> = { [K in keyof T & string as `get${Capitalize<K>}`]: () => T[K] };

function q1<T, K extends keyof T, U extends T, W extends Partial<T>>(t: T, k: K, u: U, w: W, e: {}) {
    const a1: Partial<T> = t;
    const a2: Readonly<T> = t;
    const a3: Pick<T, K> = t;
    const a4: Partial<T> = u;
    const a5: Readonly<T> = u;
    const a6: Pick<T, K> = u;
    const a7: Partial<T> = w;
    const a8: Partial<T> = {};
    const a9: Partial<T> = e;
    const b1: { [P in keyof T]: T[P] } = t;
    const b2: { [P in keyof T]?: T[P] } = u;
    const b3: Partial<Pick<T, K>> = t;
    const b4: Record<K, T[K]> = t;
    const b5: { [P in K]: T[P] } = t;
    const b6: Partial<Readonly<T>> = t;
    const b7: { readonly [P in keyof T]?: T[P] } = {};
    const b8: Getters<T> = null!;
    const b9: Partial<T> | undefined = undefined;
}

function q2<T extends { a: string; b: number }, K extends "a" | "b">(t: T, x: { a: string; b: number }, y: { a: "x"; b: 1; c: true }) {
    const a1: Partial<Record<K, unknown>> = x;
    const a2: Record<K, string | number> = x;
    const a3: Record<K, string | number> = y;
    const a4: Pick<T, "a"> = t;
    const a5: Partial<Record<K, string | number>> = y;
    const a6: Record<K, unknown> = t;
    const a7: { [P in K]?: unknown } = x;
}

function q3<K extends string, T extends Record<K, number>>(t: T, n: Record<K, number>) {
    const a1: Record<K, number> = t;
    const a2: { [P in K]: number } = n;
    const a3: Partial<Record<K, number>> = t;
}

function q4<T extends object>(t: T, arr: T[]) {
    const a1: Readonly<T> = t;
    const a2: Partial<T> = t;
    const a3: ReadonlyArray<T> = arr;
}

class C<T> {
    f(x: T) {
        const a1: Partial<this> = this;
        const a2: Readonly<this> = this;
        const a3: Pick<this, keyof this> = this;
    }
}

function q5<T extends unknown[]>(t: T) {
    const a1: Readonly<T> = t;
    const a2: Partial<T> = t;
}

function q6<T>(t: T) {
    const a1: Record<keyof T, unknown> = t as any;
    const a2: Partial<T> = null as never;
    const a3: Required<T> = null as any;
}
