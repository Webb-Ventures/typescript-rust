// `x instanceof f` where `f` has no construct signatures (a `Function`, a plain function)
// narrows to the members that are objects; `any` becomes `{}`. The false branch is kept.
declare const x: (() => void) | null;
declare const ctor: Function;
declare function plain(): void;
declare const u: string | { a: number } | null;
declare const v: any;

if (x instanceof ctor) {
    x();
} else {
    const n: null = x;
}
if (u instanceof plain) {
    const o: { a: number } = u;
} else {
    const s: string = u;
}
if (v instanceof ctor) {
    v.val;
}
export {};
