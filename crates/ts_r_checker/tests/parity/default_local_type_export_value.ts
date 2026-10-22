// @filename: comp.ts
// `export default F` of a local interface next to an exported value of its name exports
// the value too.
interface F { msg: string; }
export const F = ({ msg }: F) => msg.length;
export default F;
// @filename: other.ts
interface G { n: number; }
export default G;
// @filename: main.ts
import F from "./comp";
import G from "./other";
const n: number = F({ msg: "hi" });
const bad: string = F({ msg: "hi" });
G; const z: string = F; F.foo;
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "module": "esnext", "moduleResolution": "bundler", "noEmit": true }, "files": ["comp.ts", "other.ts", "main.ts"] }
