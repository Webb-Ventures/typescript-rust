// A namespace merged with an export specifier of its name is that namespace in qualified
// names, and an export specifier whose local name is a type import merged with a `const`
// exports the `const` as the value. An unresolved augmentation is reported once.
declare module "m2" {
    export namespace X {
        interface I { }
    }
    function Y(): void;
    export { Y as X };
    function Z(): X.I;
}
declare module "m2" {
    function Z2(): X.I;
}
// @filename: script.ts
declare module "m3" {
    export namespace X {
        interface I { }
    }
    function Y(): void;
    export { Y as X };
    function Z(): X.I;
}
declare module "m3" {
    function Z2(): X.I;
}
// @filename: t.ts
interface A {}
export type { A };
// @filename: b.ts
import { A } from "./t";
const A = 0;
export { A };
// @filename: c.ts
import { A } from "./b";
export const n: number = A;
// @filename: aug.ts
export {};
declare module "m9" { }
declare module "m9" { }
// @filename: tsconfig.json
{
    "compilerOptions": {
        "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext",
        "moduleResolution": "Bundler", "moduleDetection": "auto", "noEmit": true
    },
    "files": ["a.ts", "script.ts", "t.ts", "b.ts", "c.ts", "aug.ts"]
}
