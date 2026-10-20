// Parameters that are binding patterns: the type the pattern implies, TS7031 on each name
// that is implicitly `any`, defaults typed by the pattern, and IIFE parameters.
function f1({ a, b }) { return [a, b]; }
function f2({ a = 1, b = "" } = {}) { const z: 1 = a; const y: 1 = b; }
function f3([a, b = 1]) { const y: 1 = b; return a; }
function f4({ a: { c }, d = [] as string[] }) { return [c, d]; }
function f5({ a = 1, ...r }) { const z: 1 = a; return r; }
function f6([a, b] = [1, "x"]) { const z: 1 = a; const y: 1 = b; }
function f7({ a } = { a: 1, z: 2 }) { const z: 1 = a; }
function f8({ a: renamed, b: [x, y] }) { return [renamed, x, y]; }
function f9({} = { a: 1 }) {}
const g1 = ({ to, subject }) => to + subject;
declare const anyv: any;
new anyv({ onSend: ({ to, body }) => to });
const t1: string = f1;
const t2: string = f2;
const t3: string = f3;
const t4: string = f4;
const t8: string = f8;
f1({ a: 1 });
f1({ a: 1, b: 2, c: 3 });
f2({ a: "x" });
f3([1, 2, 3]);
const i1 = (({ kind, ...rest }) => { const z: 1 = kind; return rest; })({ kind: "a", x: 1 });
declare class Amb { private bar({ a }); }
const gt: 1 = globalThis.parseInt;
type G = typeof globalThis extends { parseInt: any } ? 1 : 2;
const gg: G = 2;
