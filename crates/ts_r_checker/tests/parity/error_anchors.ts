// Error anchors: a function or class whose call (`new`) would fit is reported at the
// expression; TS2558 over the type arguments; a type parameter's constraints one at a time.
interface Dog { bark: true }
declare function getRover(): Dog;
declare function needsArg(a: number): Dog;
declare function optArg(a?: number): Dog;
declare function cat(): string;
class K { bark: true = true }
let x: Dog = getRover;
let y: Dog = needsArg;
let z: Dog = optArg;
let w: Dog = cat;
let v: Dog = K;
let u: void = getRover;
x = getRover;
function takes(d: Dog) {}
takes(getRover);
function ret(): Dog { return getRover; }
const o: { d: Dog } = { d: getRover };
let arrow: Dog = () => ({ bark: true as const });
declare function anyRet(): any;
let q: string[] = anyRet;
declare function gen<T>(): void;
gen<  number , string  >();
getRover<number>();
function fff<T extends { (): void }, U extends T>(y: U) {
    const f: (x: string) => string = y;
}
const m: { [k: string]: string } = {
    foo() { return ""; },
};
