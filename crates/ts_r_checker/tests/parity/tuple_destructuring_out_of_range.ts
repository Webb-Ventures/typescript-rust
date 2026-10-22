// Destructuring past the end of a tuple without a rest element is TS2493 (not with a
// default): array and object patterns, declarations, parameters and assignments.
declare var array1: [number, number];
const [a, b, c = 1] = array1;
const [d, e, [f]] = array1;
const [g, h, {length}] = array1;
var [x, y, z] = [1, "hello"];
var [p, q, r] = [];
var [s, t, u] = [1];
function fn(...[a, , , d]: [boolean, string, number]) { }
function fo(...{0: a, length, 3: d}: [boolean, string, number]) { }
const {5: w} = array1;
let zz: any;
({ 5: zz } = array1);
function pp([a, b, c]: [number, number]) {}
const [...[m, n, o]] = array1;
let a1: any, b1: any, c1: any;
[a1, b1, c1] = array1;
[, , a1 = 1] = array1;
class C { t: any; u: any; v: any; m() { () => [this.t, this.u, this.v] = array1; } }
