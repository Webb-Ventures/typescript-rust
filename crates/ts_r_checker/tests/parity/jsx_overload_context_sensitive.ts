// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"jsx":"preserve","noEmit":true,"lib":["es2015"],"types":[]},"files":["a.tsx"]}
// @filename: a.tsx
declare namespace JSX { interface Element {} interface IntrinsicAttributes {} }
declare function log(...a: any[]): void;
interface ButtonProps { onClick: (k: "left" | "right") => void; }
interface LinkProps { goTo: "home" | "contact"; }
declare function MainButton(p: ButtonProps): JSX.Element;
declare function MainButton(p: LinkProps): JSX.Element;
const b2 = <MainButton onClick={(k)=>{log(k)}} extra />;
const b3 = <MainButton onClick={(k)=>{log(k)}} />;
declare function f(p: ButtonProps & {x?: 1}): void;
declare function f(p: LinkProps): void;
f({onClick: (k) => {}, extra: 1});
const b4 = <MainButton {...{onClick: (k) => {log(k)}}} extra />;
