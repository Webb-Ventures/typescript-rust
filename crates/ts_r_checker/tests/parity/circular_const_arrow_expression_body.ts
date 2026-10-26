// An arrow's expression body calling the arrow itself is circular (TS7023), also for a
// `const`; block bodies returning such a call do not count it.
const pick = () => pick();
const g = function () { return g(); };
const h = () => { return h(); };
function k() { return k(); }
var f = () => f();
export {};
