// An intersection whose literal-typed property has no common value is never; unions drop
// such members. Non-literal conflicts (`string & number`) do not empty it.
type OptionOne = { kind: "one"; s: string };
type OptionTwo = { kind: "two"; x: number; y: number };
type Options = OptionOne | OptionTwo;
type X = OptionTwo & { kind: "one" };
const x: undefined = null! as X;
type Y = Options & { kind: "one" };
const y: undefined = null! as Y;
declare const yv: Y;
yv.s;
function f(o: Options & { kind: "two" }) {
    o.x;
}
type Z = { a: string } & { a: number };
const z: undefined = null! as Z;
declare const zv: Z;
zv.a;
type W = OptionTwo & { kind: "one" | "two" };
const w: undefined = null! as W;
type V = { kind: "one", p?: string } & { kind: "two" };
const v: undefined = null! as V;
declare const xv: X;
xv.kind;
declare const vv: V;
vv.p;
type R = OptionOne & { kind: "one" | "two" };
declare const rv: R;
rv.s;
