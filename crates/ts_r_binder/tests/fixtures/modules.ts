// Import and export bindings, re-exports and module requests.
import def, { a, b as bee, type T } from "./dep";
import * as ns from "./ns";
import eq = require("./eq");
export { a as renamed, bee };
export * from "./star";
export * as nsStar from "./star2";
export const local = def + ns.x + eq;
export default function () {}
export type { T };
const lazy = import("./lazy");
