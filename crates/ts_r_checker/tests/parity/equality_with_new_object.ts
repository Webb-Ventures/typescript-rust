// TS2839: an equality with an object, array, function, class or regex literal operand
export {};
const a = { a: 1 };
const b = [1];
if ((({ a: 1 }))  === a) {}
if (a == { a: 1 }) {}
if (a != [1]) {}
if (b !== (([1]))) {}
if ({} == null) {}
if (a === { a: 1 } as any) {}
const c = a === <any>[];
function f(x: unknown) { return x === {}; }
declare const u: any;
if (u === function () {}) {}
if (u === class {}) {}
if (u === /a/) {}
if (u === new Date()) {}
if (u === `a`) {}
if (u === (() => 1)) {}
if (function () {} !== u) {}
