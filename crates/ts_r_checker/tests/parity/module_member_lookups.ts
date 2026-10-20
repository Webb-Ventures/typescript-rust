// Namespaces that re-export values are values; missing named imports get suggestions;
// `"export="` as a string in a module does not hide its `export =`; numeric keys stay numbers.
// @filename: ns.ts
declare const ___a: number;
type ___T = string;
declare namespace __ {
  export { ___a as a, type ___T as T };
}
declare namespace __t {
  export { type ___T as T };
}
declare namespace __u {
  export { ___T as T };
}
declare namespace __v {
  export type { ___a as a };
}
type P = typeof __;
const x: number = 0 as unknown as P['a'];
type Q = typeof __t;
type R = typeof __u;
type S = typeof __v;
type U = typeof ___T;
// @filename: lib.d.ts
declare function lib(): void;
declare namespace lib {
  export interface Plugin { a: 1 }
  export const parse: () => void;
  enum Names { ExportEquals = "export=" }
}
export = lib;
// @filename: d.d.ts
export default function f(): void;
export const other: number;
// @filename: a.ts
export { parse, Plugin, plugin, nope } from "./lib.js";
import { plugn, Plugin as P2 } from "./lib.js";
import { f, othr } from "./d.js";
// @filename: keys.ts
enum K { A = 1, B = 2 }
interface T { [K.A]: 'a'; [K.B]: 'b' }
interface N { 1: 'a'; [2]: 'b' }
const kx: keyof T = K.A;
const ky: keyof N = 1;
type Tok<Kind extends K> = { kind: Kind };
type Z = Tok<keyof T>;
