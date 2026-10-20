// A type met again while it is printed prints `any`, not `...`; a function's own type
// prints `typeof f`.
export {};
var f4: () => typeof f4;
f4 = 3;
declare var a27: { prop: number } | { prop: T27 };
type T27 = typeof a27;
declare var b: T27;
var s: string = b;
function f5() { return f5; }
var n: number = f5;
