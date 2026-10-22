// @filename: m.ts
export class P { }
export interface Q { }
// @filename: a.ts
import W = require('./m');
import * as N from './m';
var x = (w1: W) => { };
function z(w3: N) { }
let q: W.Q;
export {};
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "module": "commonjs", "noEmit": true }, "files": ["m.ts", "a.ts"] }
