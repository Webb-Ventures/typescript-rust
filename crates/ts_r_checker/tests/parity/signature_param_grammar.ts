// `readonly` on a type that is not an array or tuple type (TS1354), parameter properties
// with binding patterns (TS1187) and unannotated parameters of signatures in types
// (TS7006/TS7019, also `(readonly x) => void`).
type A = readonly string;
type B = readonly Set<number>;
type C = readonly string[] | readonly [number];
class K {
  constructor(public { a }: { a: number }, private [b]: number[]) {}
}
let f2: (x) => void;
let f3: new (x) => object;
let f4: (readonly x) => void;
let f5: (...xs) => void;
let f6: (this, y: number) => void;
interface I { m(a): void; (b): void; new (c): I; p: (d) => void }
type T = { m(e): void };
declare function df(g): void;
abstract class AC { abstract am(i): void; }
export {};
