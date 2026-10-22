// A `for`-`in`/`of` variable whose iterated expression needs its own type is TS7022, and a
// block-scoped one read there is used before its declaration (TS2448).
for (var v of v) { }
let w = [1];
for (let w of w) { }
for (const x of [1, 2]) { for (var y of y) {} }
declare const anything: any;
for (var z of anything) { }
const o = { a: 1 };
for (const o in o) { }
for (const q of [() => q]) { }
export {};
