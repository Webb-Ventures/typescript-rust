// `if (x = y)` and `if (x &&= y)` narrow `y`; `||=`/`??=` may skip `y`.
// The target of `x ||= []` reads its type from before the assignment.
type T = { name: string };
function g(thing: T | undefined, d: T | undefined) {
    if (thing &&= d) {
        d.name;
    } else {
        d.name;
    }
    let z: T | undefined;
    if (z = d) {
        d.name;
    } else {
        d.name;
    }
    if (thing ||= d) {
        d.name;
    }
    if (thing ??= d) {
        d.name;
    }
    if ((z = d) && thing) {
        d.name;
    }
}
function foo1(results: number[] | undefined) {
    (results ||= []).push(100);
}
function foo2(results: number[] | undefined) {
    (results ??= []).push(100);
}
function foo3(results: number[] | undefined) {
    (results &&= []).push(100);
}
