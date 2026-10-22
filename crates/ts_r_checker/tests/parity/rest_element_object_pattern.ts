// An object pattern as the rest element of an array pattern reads the rest array (or the
// rest of a tuple); a property it lacks is TS2339.
var [...{0: a, b }] = [0, 1];
const [...{ length, c }] = ["x"];
const [x, ...{ 0: y, d }] = [1, "s"] as [number, string];
function f([...{ e }]: number[]) {}
let g: number, h: number;
[...{ 0: g, h }] = [1, 2];
