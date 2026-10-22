// A property discriminates a union only when the types of the members that have it
// differ and one of them is a literal type: an optional method present in one member
// does not narrow by truthiness.
declare const z: { b: 1 } | { a?: () => void };
if (z.a) {
    const t: string = z;
}
declare const k: { kind: "a"; x: number } | { kind: "b"; y: string };
if (k.kind === "a") {
    const t: string = k;
}
declare const o: { v?: "x"; p: 1 } | { v: number; q: 2 };
if (o.v) {
    const t: string = o;
}
declare const s: { f: string; g: 1 } | { f: number; h: 2 };
if (typeof s.f === "string") {
    const t: string = s;
}
export {};
