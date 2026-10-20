interface A { [x: string]: any; }
interface A { [y: string]: any; }
class C { [x: string]: any; static [x: string]: any; }
class D { static [x: string]: any; static [y: string]: number; }
interface E { [x: 'a']: any; [x: 'a']: any; }
interface F { [x: Date]: any; [x: Date]: any; }
type G = { [x: string]: any } & { [x: string]: any };
interface H { [x: string | number]: any; [x: number]: any; [x: string]: any; }
interface I2 { [x: string]: any; [x: string, y: number]: any; }
interface J { [x: `a${string}`]: any; [x: `a${string}`]: any; }
type U = string;
interface K { [x: U]: any; [x: string]: any; }
class L { [x: string]: any; }
interface L { [x: string]: any; }
interface M<T extends string> { [x: T]: any; [x: T]: any; }
interface N { [x: string & {b: 1}]: any; [x: string & {b: 1}]: any; }
interface O { [x]: any; [x]: any; }
interface P { [x: string]: any; [x: number]: any; }
export {};
