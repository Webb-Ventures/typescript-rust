// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"jsx":"preserve","noEmit":true,"lib":["es2015"],"types":[]},"files":["a.tsx"]}
// @filename: a.tsx
declare namespace JSX { interface Element {} interface IntrinsicElements { div: {} } }
declare function f<T>(): void;
f<number,>();
const a = <div<number,>></div>;
declare function C<T>(p: {}): JSX.Element;
const b = <C<number,> />;
type X = Array<number,>;
