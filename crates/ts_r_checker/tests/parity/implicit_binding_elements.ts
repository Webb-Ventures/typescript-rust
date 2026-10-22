// TS7031 for binding elements that are implicitly `any`: names of an array pattern past
// its default's tuple elements (unless they have a default), an array pattern's rest, and
// the names of a variable's pattern without annotation or initializer.
function f00([x, y]) {}
function f01([x, y] = []) {}
function f02([x, y] = [1]) {}
function f03([x, y] = [1, 'foo']) {}
function f10([x = 0, y]) {}
function f11([x = 0, y] = []) {}
function f12([x = 0, y] = [1]) {}
function f13([x = 0, y] = [1, 'foo']) {}
function f14([x, y = 1] = []) {}
function a10([a, b, [[c]], ...x]) { }
function f6([f, ...g]) { }
function typed([a, ...b]: number[]) { }
var [a1], {b1} = { b1: 1 };
declare const anything: any;
for (const [k, v] of anything) { k; v; }
export {};
