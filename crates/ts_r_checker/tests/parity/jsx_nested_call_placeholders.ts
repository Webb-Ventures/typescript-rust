// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"jsx":"preserve","noEmit":true,"target":"es2015","lib":["es2015","dom"],"noLib":false,"types":[]},"files":["a.tsx"]}
// @filename: a.tsx
declare namespace JSX { interface Element {} }
declare const a: any;
declare function inner(a: 1, p: { cb: number }): 2;
declare function inner(a: 1, p: { cb: (x: string) => number }): 2;
declare function inner(a: 1, p: { cb: (x: any) => any }): 3;
declare function Outer(p: { run: (x: number) => void; value: 2 }): JSX.Element;
declare function Outer(p: { run: (x: string) => void; value: 3 }): JSX.Element;
const el = <Outer run={x => {}} value={inner(a, { cb: (x = a) => x })} />;
