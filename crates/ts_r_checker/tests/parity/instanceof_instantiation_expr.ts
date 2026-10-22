declare class Box<T> { value: T; }
declare const u: unknown;
u instanceof Box;
u instanceof Box<number>;
u instanceof ((Box<string>));
Box<number> instanceof Object;
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
