// An object literal initializing an object binding pattern (variable or parameter
// default) may only name properties the pattern binds (TS2353), checked through nested
// patterns, with computed pattern keys of literal type; defaults are not type-checked here.
export {};
var { y1 } = { x1: 1 };
var { y2 = "s" } = { y2: 1 };
var [{ a6 }] = [{ a6: 1, b6: 2 }];
var { y7: [{ a7 }] } = { y7: [{ a7: 1, b7: 2 }] };
var { y8: { a8 } } = { y8: { a8: 1, b8: 2 } };
var { y9 } = ({ y9: 1, z9: 2 });
var { y10 } = { y10: 1, z10: 2 } as const;
function f11({ a11 } = { a11: 1, b11: 2 }) {}
for (var { a12 } = { a12: 1, b12: 2 }; ;) break;
var { y13 = 1 } = { y13: undefined, z13: 1 };
let { w, ...rest } = { w: 1, z: 2 };
const named = "prop";
let { [named]: computed } = { prop: "b" };
const notPresent = "prop2";
let { [notPresent]: computed2 } = { prop: "b" };
const nn = 3;
let { [nn]: c3 } = { 3: 1, 4: 2 };
