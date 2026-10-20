// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "strict": true, "target": "ES2022", "module": "ESNext"}, "files": ["a.js"]}
// @filename: a.js
// `@readonly` on a method, accessor or constructor: TS1024 at the tag. On a constructor,
// `@template` is TS1092 at the tag and `@returns {T}` TS1093 at `T`. A class method's
// `@type` without a fitting call signature is TS8030 (not a getter's).
export class C {
    /** @readonly */
    constructor() {}
    /** @readonly */
    m() {}
    /** @readonly */
    get g() { return 1; }
    /** @readonly */
    static s = 1;
    /** @readonly */
    #p = 1;
}
export class D {
    /** @template T, U */
    constructor() {}
}
export class E {
    /**
     * @param {number} a
     * @returns {number}
     */
    constructor(a) {}
}
export class F {
    /** @return {string} */
    constructor() {}
    /** @type {string} */
    m() { return ""; }
    /** @type {(a: number) => string} */
    n(a) { return ""; }
    /** @type {() => string} */
    o(a, b) { return ""; }
    /** @type {string} */
    get g() { return ""; }
}
/** @readonly */
export function f() {}
/** @readonly */
export const v = 1;
