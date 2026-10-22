// @filename: a.ts
import { foo, bar } from "./helpers";
const local = "l";
enum A {
    a = foo,
    b,
    c = bar,
    d = local,
    e = `${foo}`,
    f = "" + bar,
}
enum B {
    a = (foo),
    b = 1,
    c,
}
export {};
// @filename: helpers.ts
export const foo = 2;
export const bar = "bar";
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true, "isolatedModules": true }, "files": ["a.ts", "helpers.ts"] }
