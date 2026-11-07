// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"jsx":"preserve","noEmit":true,"lib":["es2015"],"types":[]},"files":["a.tsx"]}
// @filename: a.tsx
declare namespace JSX {
    export interface Element { e: 1 }
    export interface IntrinsicElements { div: { a?: number; b?: string } }
}
declare const n: number;
declare const o: { a: number };
const x1 = <div {...n} />;
const x2 = <div a={1} {...o} />;
const x3 = <div {...{ a: 1 }} a={2} />;
const x4 = <div {...{ ...n }} />;
const x5 = <div {...{ a: 1, ...o }} />;
const x6 = <div {...(true && o)} />;
declare const u: undefined;
const x7 = <div {...u} />;
const y1 = { ...n };
const y2 = { a: 1, ...o };
const y3 = { ...u };
