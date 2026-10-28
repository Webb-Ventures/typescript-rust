// `export { x }` in an ambient module may name a global declared in a `global` block of
// another declaration of the module (as @types/node does for Buffer). A global from a
// script's top level is still TS2661.
// @filename: a.d.ts
declare module "m" {
    global {
        interface Shared { a: number }
        var Shared: { make(): Shared };
        type Alias = string;
    }
}
declare var TopLevel: number;
// @filename: b.d.ts
declare module "m" {
    export { type Alias, Shared, TopLevel };
}
// @filename: c.ts
import { Shared, type Alias } from "m";
const s: Shared = Shared.make();
const t: Alias = s.a;
