// Checks read from the syntax alone: TS2695 (comma left side without side effects),
// TS2872/TS2873 (condition always truthy/falsy), TS2869/TS2871 (`??` left side never/always
// nullish).
declare let x: any;
declare let o: any;
declare function f(): any;
x = ((1, 2), 3);
x = (f() + 1, 3);
x = (-f(), 3);
x = (x ? f() : 1, 3);
x = (x = 1, f(), 3);
x = (0, o.p)();
x = (0, eval)("1");
x = (1, o.p)();
x = (0, o.p);
x = (this, o.p);
x = (f()!, 3);
x = (function named() {}, 3);
if ({}) {}
if (0 || 1) {}
if ("") {}
if (`a${x}`) {}
if (0n) {}
if (2) {}
if (!{}) {}
if (x ? {} : []) {}
if ({} as any) {}
while ({} satisfies unknown) {}
while (1) {}
const c1 = function named() {} || undefined;
const c2 = (class K {}) && null;
const n1 = null ?? null ?? 1;
const n2 = x ?? 1 ?? 2;
const n3 = (x, null) ?? 1;
const n4 = (x && 1) ?? 1;
const n5 = new Date() ?? 1;
const n6 = (<any>null) ?? 1;
const n7 = `a${x}` ?? 1;
