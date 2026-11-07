// A tag is resolved like a callee, with tsc's tagged-template differences: its receiver is
// the `this` argument (TS2684, TS2741); it is not checked for null or undefined first (a
// possibly undefined tag is TS2349 at the whole tag, not TS2722); an untyped `Function` tag
// takes type arguments without TS2347.
export {};
const obj = { m(this: { n: number }, s: TemplateStringsArray) { return this.n; } };
obj.m`x`;
obj["m"]`x`;
declare const o2: { f(this: string, s: TemplateStringsArray): void };
o2.f`x`;
(o2.f)`x`;
declare function vt(this: string, s: TemplateStringsArray): void;
vt`x`;
declare const ov: {
    f(this: string, s: TemplateStringsArray, a: number): void;
    f(this: number, s: TemplateStringsArray, a: string): void;
};
ov.f`${1}`;
class K { n = 1; t(this: K, s: TemplateStringsArray) {} q() { this.t`x`; } }
declare const maybe: { t(s: TemplateStringsArray): void } | undefined;
maybe?.t`x`;
declare const mu: ((s: TemplateStringsArray) => void) | undefined;
mu`x`;
declare const un: undefined;
un`x`;
declare const uk: unknown;
uk`x`;
declare const o: { t: number; m?: (s: TemplateStringsArray) => void };
o.t`x`;
o.m`x`;
let fn: Function = () => {};
fn<number>`x`;
fn<Missing>`x`;
type F = (this: string, s: TemplateStringsArray) => void;
declare const o3: { f: F };
(o3.f satisfies F)`x`;
(o3.f as F)`x`;
(o3.f as F)(null!);
