// date-fns `convertToFP/test.ts`: under nodenext in ESM mode, a package subpath without
// `exports` that is a directory with its own `package.json` uses its `typings`.
import { pipe } from "pk/function";
const n: string = pipe(1);
// @filename: package.json
{ "name": "p", "type": "module" }
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "esnext", "module": "nodenext", "moduleResolution": "nodenext", "moduleDetection": "force", "noEmit": true, "skipLibCheck": true}, "files": ["a.ts"]}
// @filename: node_modules/pk/package.json
{ "name": "pk", "main": "./lib/index.js", "typings": "lib/index.d.ts" }
// @filename: node_modules/pk/function/package.json
{ "main": "../lib/function.js", "typings": "../lib/function.d.ts" }
// @filename: node_modules/pk/lib/function.d.ts
export declare function pipe<A>(a: A): A;
