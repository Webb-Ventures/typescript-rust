// A rest parameter at or past a contextual rest parameter takes the rest type sliced at
// its position: a tuple from the matching element on, an array of a generic rest's
// element past it.
declare const t1: [number, boolean, string];
declare function f1(cb: (...args: typeof t1) => void): void;
f1((a, ...x) => { const n: [boolean, string] = x; });
f1((a, b, ...x) => { const n: [string] = x; });
declare const t3: [boolean, ...string[]];
declare function f3(cb: (x: number, ...args: typeof t3) => void): void;
f3((a, b, ...x) => { const n: string[] = x; });
function f4<T extends any[]>(t: T) {
    function f(cb: (x: number, ...args: T) => void) {}
    f((...x) => {});
    f((a, ...x) => { const n: T = x; });
}
f1((a, ...x) => { const bad: [string] = x; });
