// `resolveJsonModule`: a JSON module is its value, widened (default and namespace
// imports alike).
import data from './data.json';
import * as ns from './data.json';
import list from './list.json';
const a: boolean = data;
const b: boolean = data.arr;
const c: boolean = data.nested;
const d: boolean = data.empty;
const e: boolean = ns.name;
const f: boolean = list;
// @filename: data.json
{
  "name": "x",
  "n": 1,
  "arr": [1, "a", {"k": true}],
  "nested": {"deep": [null, 2]},
  "empty": [],
  "neg": -1
}
// @filename: list.json
[{"id": 1}, {"id": 2, "tag": "t"}]
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "resolveJsonModule": true}, "files": ["a.ts"]}
