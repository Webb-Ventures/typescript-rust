// Modifiers on import declarations: TS1079 for `declare`, else TS1191 for `export`.
export const v = 1;
export namespace M { export var m = 1; }
export import { v as a } from "./a";
export declare import { v as b } from "./a";
declare export import { v as c } from "./a";
export async import { v as d } from "./a";
declare import e = M;
export declare import f = M;
import g = M;
export { a, b, c, d, e, g };
