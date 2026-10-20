// A named import from an `export =` module whose value is a `const` reads that value's
// property (`import { resolve } from "node:path"`), also through a re-exporting module.
import { resolve } from 'node:pth';
import { resolve as r2, sep } from 'pth';
export const t: number = resolve;
export const t2: number = r2;
export const t3: number = sep;
r2.foo;
export const dir: string | undefined = undefined as string | undefined;
export const joined = resolve(dir, 'x');

// @filename: pth.d.ts
declare module "pth" {
    namespace pth {
        interface PlatformPath {
            resolve(...paths: string[]): string;
            sep: "/" | "\\";
        }
    }
    const pth: pth.PlatformPath;
    export = pth;
}
declare module "node:pth" {
    import pth = require("pth");
    export = pth;
}
