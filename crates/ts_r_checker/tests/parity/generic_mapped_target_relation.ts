type Model = { a: string; b: number };
type MM<S extends string> = { [K in keyof Model as `${K}${S}`]: Model[K] };
type Box<T> = { [P in keyof T]: { v: T[P] } };

function p1<T extends string, U>(u: U, n: number, s: "x", o: { a: 1 }, arr: number[], b: boolean, idx: { [k: string]: number }, f: () => void) {
    const a1: Partial<U> = n;
    const a2: Partial<U> = s;
    const a3: Partial<U> = o;
    const a4: Partial<U> = arr;
    const a5: Partial<U> = b;
    const a6: Partial<U> = idx;
    const a7: Partial<U> = f;
    const b1: Readonly<U> = n;
    const b2: Readonly<U> = o;
    const b3: Readonly<U> = idx;
    const b4: Readonly<U> = arr;
    const c1: Box<U> = n;
    const c2: Box<U> = o;
    const c3: Box<U> = idx;
    const c4: Box<U> = {};
    const d1: Record<T, number> = n;
    const d2: Record<T, number> = o;
    const d3: Record<T, number> = idx;
    const d4: Record<T, number> = {};
    const e1: MM<T> = idx;
    const e2: MM<T> = o;
    const e3: MM<T> = {};
    const e4: Partial<MM<T>> = {};
}

function p2<T, K extends keyof T, V>(x: { [k: string]: V }, y: { a: V }, t: T, v: V) {
    const a1: Partial<T> = x;
    const a2: Partial<T> = y;
    const a3: Record<K, V> = x;
    const a4: Record<K, V> = y;
    const a5: Pick<T, K> = y;
    const a6: Pick<T, K> = x;
    const a7: Readonly<T> = v;
    const a8: Partial<T> = v;
    const a9: Required<T> = v;
    const b2: Box<T> = v;
    const b3: { [P in keyof T]: T[P] } = t;
    const b4: { [P in keyof T & string as `p_${P}`]: T[P] } = t;
    const b5: { [P in K]: T[P] } = t;
}

function p3<T extends { a: string }, K extends string>(t: T, k: K, x: { a: string; b: number }) {
    const a1: Partial<T> = x;
    const a2: Readonly<T> = x;
    const a3: Record<K, string> = x;
    const a4: Record<K, string> = t;
    const a5: Partial<Record<K, string>> = x;
    const a6: Partial<Record<K, number>> = x;
}
