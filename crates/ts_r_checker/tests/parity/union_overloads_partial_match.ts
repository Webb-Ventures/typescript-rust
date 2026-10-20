// trpc `res.end(JSON.stringify(x))` on `ServerResponse | Http2ServerResponse`: union call
// signatures come from every member's overloads; a signature matches another member's that
// is identical or takes no more required arguments with parameters it is a subtype of.
declare const a: { f(cb?: () => void): 1; f(x: any, cb?: () => void): 1 } | { f(cb?: () => void): 2; f(x: string, cb?: () => void): 2 };
const a1: null = a.f("x");
declare const b: { f(cb?: () => void): 1; f(x: number, cb?: () => void): 1 } | { f(cb?: () => void): 2; f(x: string, cb?: () => void): 2 };
b.f("x");
declare const c: { f(x: any): 1; f(x: any, y: number): 1 } | { f(x: string): 2; f(x: number, y: number): 2 };
c.f("x");
c.f(1, 2);
declare const d: { f(cb: () => void): 1; f(x: number): 1 } | { f(cb: () => void): 2; f(x: string): 2 };
d.f("x");
declare const e: { f(): 1; f(x: number): 1 } | { f(): 2; f(x: string): 2 };
const e1: null = e.f("x");
const e2: null = e.f(true);
declare const g: { f(): 1; f(x: number): 1 } | { f(): 2; f(x: number, y: string): 2 };
const g1: null = g.f(1, "a");
const g2: null = g.f(1, 2);
declare const h: { f(): 1; f(x: number): 1 } | { f(): 2; f(x: number): 2; f(x: boolean): 2 };
const h1: null = h.f(true);
declare const p: { f(x: string): 1; f(x: number): 1 } | { f(x: "a"): 2; f(x: 5): 2 };
const p1: null = p.f("b");
declare const q: { f(x: "a"): 1; f(x: 5): 1 } | { f(x: string): 2; f(x: number): 2 };
const q1: null = q.f("b");
declare const r: { f(x: string, y?: number): 1; f(x: number): 1 } | { f(x: string): 2; f(x: boolean): 2 };
const r1: null = r.f("b", 1);
declare const s: { f(x: string): 1; f(x: number): 1 } | { f(x: string, y?: number): 2; f(x: boolean): 2 };
const s1: null = s.f("b", 1);
export {};
