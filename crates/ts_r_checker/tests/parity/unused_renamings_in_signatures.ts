// A renamed destructured property in a signature without a body is probably a type.
type O = { a?: string; b: number; c: number; };
type F2 = ({ a: string }: O) => any;
type F6 = ({ a: string }) => typeof string;
type F7 = ({ a: string, b: number }) => typeof number;
type F11 = ({ 2: string }) => void;
type F12 = ({ ["a"]: string }: O) => void;
type G2 = new ({ a: string }: O) => any;
type T3 = ([{ a: b }, { b: a }]) => void;
type T4 = ([{ a: [b, c] }]) => void;
interface I {
    method2({ a: string }): any;
    ({ a: string }): any;
    new ({ a: string }): any;
}
declare function f({ a: number }): void;
function g({ a: string }: O) { }
const h = ({ a: string, b, c }: O) => { };
export {};
// @filename: tsconfig.json
{
    "compilerOptions": {
        "strict": false, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext",
        "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true
    }
}
