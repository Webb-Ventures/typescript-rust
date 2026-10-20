// @target: es2022
// @module: commonjs
// Locals exported by an export list stay in sync when they are reassigned.
let x = 0, z = 0;
export { x, x as y, z };
x = 1;
x += 2;
x++;
++x;
x--;
let w = x++;
w = (x = 5);
function f() { x = 9; }
z = x;
