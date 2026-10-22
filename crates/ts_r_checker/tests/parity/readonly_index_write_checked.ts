// Writing through a readonly index signature is TS2542, and the value is still checked
// against the signature type.
class C {
    static readonly [s: string]: number;
    static readonly [s: number]: 42
}
C["foo"] = 1
C.bar = 2;
C[42] = 42
C[2] = 2;
const a = Object.freeze([1, 2, 3]);
a[0] = a[2].toString();
a[1] += 1;
declare const r: { readonly [k: string]: string };
r.x = 1;
r["y"] = 1;
