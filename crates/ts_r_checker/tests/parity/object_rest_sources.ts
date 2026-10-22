enum E { A, B }
function f<T extends { b: string }, U>(t: T, u: U, k: keyof T, i: T["b"], e: E) {
    const { ...r1 } = t;
    const { ...r2 } = u;
    const { ...r3 } = k;
    const { ...r4 } = i;
    const { ...r5 } = e;
    const { ...r6 } = t as T & { a: number };
    const { ...r7 } = 1 as { a: number } | number;
    const { ...r8 } = undefined;
    const { ...r9 } = null as { a: number } | null;
    const { ...r10 } = "s" as unknown;
    return [r1, r2, r3, r4, r5, r6, r7, r8, r9, r10];
}
function g({ ...rest }: string) {
    return rest;
}
