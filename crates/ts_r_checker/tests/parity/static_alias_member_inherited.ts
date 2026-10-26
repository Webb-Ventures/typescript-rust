// A merged namespace's `export { X as y }` is an alias: a base class's static `y` replaces it
// on the derived class's static side (postcss's `default` re-exports).
export {};
declare namespace P2 { class Q2 {} export { Q2 as default } }
declare class P2 {}
declare namespace R2 { abstract class S2 { s: 1 } export { S2 as default } }
declare class R2 extends P2 {}
const a: number = R2.default;
const a2: number = P2.default;
type K = keyof typeof R2;
const k: K = "zz";
declare namespace N2 { abstract class S2 { s: 1 } export { S2 as default } }
const d: number = N2.default;
type K2 = keyof typeof N2;
const k2: K2 = "zz";
declare namespace R5 { abstract class S5 { s: 1 } export abstract class d {} }
declare namespace P5 { export class d {} }
declare class P5 {}
declare class R5 extends P5 {}
