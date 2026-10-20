// Binding element defaults: checked against annotated declarations, part of the type
// (subtype-reduced) otherwise.
interface P { margin?: { top?: string; left?: string }; n?: number; s?: { a: number } }
declare const o: P;
// Annotated declarations: the default is checked against the element type.
function g({ n = "x" }: P) { return n; }
const { n = "x" }: P = o;
let { n: n2 = "x" }: P = o;
const [a = "x"]: number[] = [];
function g2({ s = { a: "x" } }: P) { return s; }
function g3({ s: { a } = { a: "x" } }: P) { return a; }
function g4([b = "x"]: [number?]) { return b; }
function g5({ n = (() => "x")() }: P) { return n; }
function g6({ n = "x" }: P) { const r: number = n; return r; }
function g7({ n = undefined as number | undefined }: P) { const r: number = n; return r; }
function g8({ margin = {} }: P) { return margin.left; }
type Q = { v?: "a" | "b" };
function g9({ v = "a" }: Q) { const r: "a" = v; return r; }
// Unannotated: the element type is the declared type and the default, subtype-reduced,
// literals kept for const.
const { margin = {} } = o;
const t = margin.top;
const { n: n3 = "x" } = o;
const q: number = n3;
let [z = {}] = [] as ({ a?: 1 } | undefined)[];
z.a;
let { n: n4 = "y" } = o;
const q2: number = n4;
const { margin: m3 = [] } = o;
declare function cb(f: (p: P) => void): void;
cb(({ n = "z" }) => { const r: number = n; });
