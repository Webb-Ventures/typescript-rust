// @target: esnext
// @module: commonjs
declare var _: any;
var a = 1;
var a = 2;
if (_) var b = 3;
for (var c = 1, d = 2; ;) break;
for (var e of _) { _; }
for (var {f, g} of _) _;
label: var h = 1;
{ var {i, j: [k]} = _; }
try { var l = 1; } catch { var m = 2; }
function q() { var a = 5; }
export { a, b, c, e, f, h, i, k, l, m };
