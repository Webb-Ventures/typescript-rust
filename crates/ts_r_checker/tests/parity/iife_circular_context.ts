// An argument of an immediately invoked function that types one of its unannotated
// parameters has no contextual type: that parameter is still being resolved.
// @strict: true
let twelve = (f => f(12))(i => i);
let eleven = (o => o.a(11))({ a: function(n) { return n; } });
let ten = ((f: (x: number) => number) => f(12))(i => i);
let nine = ((x) => x)({ a: 1 });
const n: number = nine.a;
export {};
