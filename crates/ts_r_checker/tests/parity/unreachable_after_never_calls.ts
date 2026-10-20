// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "strict": true, "allowUnreachableCode": false, "target": "ES2022", "module": "ESNext"}, "files": ["a.ts", "b.js"]}
// @filename: a.ts
// allowUnreachableCode: false: statements after a call returning `never` (through a dotted
// name whose symbol has an explicit type) or an exhaustive switch are TS7027, once per run.
declare function fail(): never;
const failArrow = (): never => { throw new Error(); };
const failTyped: () => never = () => { throw new Error(); };
declare const ns: { fail(): never };
export function f1(x: number) {
    if (x) {
        fail();
        x++;
        x--;
    }
    if (x > 1) {
        failArrow();  // no effect: no explicit type
        x++;
    }
    if (x > 2) {
        failTyped();
        x++;
    }
    if (x > 3) {
        [fail][0]();  // no effect: not a dotted name
        x++;
    }
    if (x > 4) {
        ns.fail();
        if (x) {
            x++;
        }
        x--;
    }
    return x;
}
export function f2(b: boolean) {
    switch (b) {
        case true: return 1;
        case false: return 0;
    }
    b;
}
// @filename: b.js
/**
 * @returns {never}
 */
function fail() {
    throw new Error();
}
/** @type {(check: boolean) => asserts check} */
const assert = check => {
    if (!check) throw new Error();
};
/** @param {*} x */
export function g(x) {
    assert(typeof x === "string");
    x.length;
    fail();
    x;
}
