import { a, b as c } from "./x";
import d, * as ns from "./y";
import "./z";
export { a, c as e };
export * from "./w";
export * as ns2 from "./w";
export default function f() { return a + c + d + ns; }
export const g = 1, h = 2;
export class K {
    static s = 1;
    #p = 2;
    get q() { return this.#p; }
    set q(v) { }
    static { K.s++; }
}
//# sourceMappingURL=mods.js.map