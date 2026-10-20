// The keys of a class, interface or aliased type print as `keyof A`, alone or in a union;
// an anonymous type's keys print as their literals.
export {};
class A { bar = 1; baz = 2; #p = 3; }
interface I { x: number; y: string }
type O = { m: 1; n: 2 };
let a: keyof A = "nope";
let i: keyof I = "nope";
let o: keyof O = "nope";
let anon: keyof { p: 1; q: 2 } = "nope";
let u: keyof I | undefined = "nope";
declare let k: keyof I;
let s1: "x" = k;
const r: Record<keyof I, number> = { x: 1 };
type M = { [P in keyof I]: boolean };
const mm: M = { x: true, y: 1 };
function f(key: keyof I) { if (key !== "x") { const z: "q" = key; } }
let arr: (keyof I)[] = ["z"];
