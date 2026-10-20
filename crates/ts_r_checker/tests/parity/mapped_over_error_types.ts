// Pick/Omit over a type from a missing module stay mapped types (not silent any), and
// interfaces extending `Omit<any, K> & X` inherit the symbol index signature too.
import { Thing } from "missing-module";
import * as script from "./globals";
import "./globals";

type O = Omit<Thing, "a">;
declare const o: O;
const n: number = o.anything;
const cb: (handler: (e: string) => void) => void = (h) => h("x");
function take(p: Omit<Thing, "kind"> & { onPick: (v: number) => void }) {
  return p;
}
take({ onPick: (v) => v.toFixed() });

type Base = { b: number };
export interface Derived extends Omit<any, "radius">, Base {
  extra: string;
}
type Both = Omit<any, "radius"> & Base;
export interface Derived2 extends Both {
  x: Omit<Thing, "y">;
}
export const s = script;
export { n, cb };
// @filename: globals.d.ts
declare var someGlobal: number;
