// @target: es2022
// @module: esnext
// An import and a local declaration of the same name (TS2440): references mean the local,
// so the import is elided (esbuild oracle: TSImportVsLocalCollisionAllTypes/Mixed).
import { a, b, c, d, e, real } from './other';
let a;
const b = 0;
var c;
function d() {}
class e {}
console.log(a, b, c, d, e, real);
