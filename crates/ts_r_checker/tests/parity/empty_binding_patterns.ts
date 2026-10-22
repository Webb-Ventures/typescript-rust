declare const u: unknown;
declare const n: null;
declare const un: string | undefined;
declare const a: any;
const {} = u;
const [] = u;
const {} = n;
const {} = un;
const {} = a;
const [] = un;
function f({}: unknown, []: unknown) {}
let x: unknown;
({} = u);
([] = u);
for (const {} of [u]) {}
export {};
