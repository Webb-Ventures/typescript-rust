interface Src { index?: number; groups?: { [k: string]: string }; }
interface Dst { index: number; groups?: { [k: string]: string }; }
interface Plain { index: number; }
declare let src: Src;
declare function take(d: Dst): void;
export let d: Dst = src;
export let p: Plain = src;
take(src);
type N = number;
type U = number;
export type Z = U<N>;
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "exactOptionalPropertyTypes": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true}, "files": ["a.ts"]}
