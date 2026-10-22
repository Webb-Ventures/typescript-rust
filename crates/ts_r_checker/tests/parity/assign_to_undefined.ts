// The global `undefined` is not a variable: assigning to it (directly, chained or in a
// destructuring target) is TS2539; `++undefined` reports only that too.
var x = undefined = null;
undefined = undefined;
[undefined] = [1];
let n = 0;
++undefined;
n++;
