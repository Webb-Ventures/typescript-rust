// @target: es2022
// @module: commonjs
// @useDefineForClassFields: false
// header comment
import fs from "fs";
// before b
import { b } from "./b";
/** doc */
export const x = fs.readFileSync(b);
// before enum
enum E { A }
class K {
    y = 1;
    constructor() {
        // in ctor
        f();
    }
}
