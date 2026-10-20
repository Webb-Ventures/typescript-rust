// The value of a late-bound expando assignment has no contextual type.
function g() {}
const k = "cb";
g[k] = (x) => 1;
export const n: string = g.cb;
