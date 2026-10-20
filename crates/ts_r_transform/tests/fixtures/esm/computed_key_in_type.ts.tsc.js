// @target: es2022
// @module: esnext
// A computed key inside a type references a value: tsc keeps its import, except in ambient
// code (`declare`) and `typeof` (ecosystem: immer src/types/types-internal.ts).
import { A } from "./a";
import { B } from "./b";
import { C } from "./c";
import { D } from "./d";
import { H } from "./h";
import { J } from "./j";
import { K } from "./k";
import { L } from "./l";
import { M } from "./m";
import { N } from "./n";
export class Q {
    p = null;
    m(x) { return x; }
}
export const v = id(null);
function id(x) { return x; }
