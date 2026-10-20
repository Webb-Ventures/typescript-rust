// Parameter initializers: only in implementations (TS2371), not reading themselves or
// later parameters (TS2372/TS2373), and instance property initializers not reading
// constructor locals (TS2301; class fields are native under ES2023 here, so it is silent).
function over(a: string): void;
function over(a = 3): void;
function over(a: any = null) { }
interface I {
    m(a = 1): void;
    n({ a = 1 }: { a?: number }): void;
}
type F = ([a = 1, { b = 2 }]: [number?, { b?: number }?]) => unknown;
declare class D {
    m(a = 1): void;
}
function self(x: number = x) {}
function later(y = { z }, z = 1) {}
function iife(y = (() => z)(), z = 1) {}
function deferred(y = () => z, z = 1) {}
function asyncIife(y = (async () => z)(), z = 1) {}
function computed(y = { [z]() { return z; } }, z = "k") {}
function typeOnly(y = { x: <typeof z>(null as any) }, z = 1) {}
function staticField(a = class { static s = b }, b = "k") {}
function assigned(a = (b = 2), b = 1) {}
(function ([c, d = c]) { })([1]);
(function ([f, g = f, h = i, i = f]) { })([1]);
const outer = 1;
function shadowed(a = outer) { var outer = 2; return a + outer; }
var aaa = 1;
class CCC {
    y: number = aaa;
    constructor(aaa: number) { }
}
