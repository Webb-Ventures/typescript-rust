// @target: es2022
// @module: esnext
// A computed key inside a type references a value: tsc keeps its import, except in ambient
// code (`declare`) and `typeof` (ecosystem: immer src/types/types-internal.ts).
import { A } from "./a";
import { B } from "./b";
import { C } from "./c";
import { D } from "./d";
import { E } from "./e";
import { F } from "./f";
import { G } from "./g";
import { H } from "./h";
import { I } from "./i";
import { J } from "./j";
import { K } from "./k";
import { L } from "./l";
import { M } from "./m";
import { N } from "./n";
import type { O } from "./o";
export type TA = { [A]: number } & Array<{ [B.x]?: 1 }>;
export interface IC { [C](): void; readonly [D]: string }
export type TE = typeof E;
declare let f: { [F]: 1 };
declare namespace NG { type T = { [G]: 1 } }
namespace NH { export type T = { [H]: 1 } }
export abstract class Q {
  declare [I]: number;
  abstract [J](): void;
  p: { [K]: 1 } = null!;
  m(x: { [L]: 1 }): { [M]: 1 } { return x as any; }
}
export const v = id<{ [N]: 1 }>(null!);
export type TO = { [O]: 1 };
function id<T>(x: T) { return x; }
