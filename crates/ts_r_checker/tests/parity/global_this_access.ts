// Missing names on `globalThis` are implicitly `any` (TS7017 under noImplicitAny), except
// global `let`/`const` declarations, which are not properties of the global object.
declare global {
    var gv: number;
    let gl: number;
}
globalThis.gv;
globalThis.gl;
globalThis.nope;
globalThis.nope2 = 1;
export {};
