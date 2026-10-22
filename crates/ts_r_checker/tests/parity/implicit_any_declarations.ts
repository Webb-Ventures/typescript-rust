// An unannotated, uninitialized variable is TS7005 at its name when its type is `any`
// rather than evolving: ambient declarations and `const`s. Ordinary `let`/`var` evolve, and
// `for`-`in`/`of` variables take the iterated type.
declare var bar;
declare namespace m {
    var x;
    var y: any;
    namespace n {
        var z;
    }
}
const c;
let evolving;
evolving = 1;
declare const anything: any;
for (const k of anything) { k; }
for (const k in anything) { k; }
export {};
