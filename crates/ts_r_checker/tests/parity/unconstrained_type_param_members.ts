// Under strictNullChecks an unconstrained type parameter has no members: its apparent type
// is `unknown`. Too few explicit type arguments still instantiate the signature.
export {};
function f1<T>(t: T) { t.toString(); t.hasOwnProperty("x"); t.foo; }
function f2<T extends unknown>(t: T) { t.toString(); }
function f3<T extends {}>(t: T) { t.toString(); }
function f4<T>(t: T | { data: T }) { t.hasOwnProperty("x"); }
function f5<T, U extends T>(u: U) { u.toString(); }
function f6<T>(t: T) { if (t) { t.toString(); } }
function f7<T>(t: T) { t?.toString(); }
function f8<T>(t: NonNullable<T>) { t.toString(); }
function f9<T>(t: T) { return `${t}`; }
function f10<T>(t: T) { t["toString"]; }
declare function map<T, U>(xs: T[], f: (x: T) => U): U[];
var r7b = map<number>([1, ""], (x) => x.toString());
