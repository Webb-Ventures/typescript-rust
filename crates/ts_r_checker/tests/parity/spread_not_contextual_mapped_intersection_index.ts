// An array spread is not contextually typed: `[...['y']]` spreads a `string[]`. A spread
// array literal is elaborated as a tuple, a spread array (a lone rest) is not.
type XY = 'x' | 'y';
declare let temp: number[];
declare let temps: string[];
var a1: [string, string] = [...temp];
var a2: string[] = [...temp];
var a3: XY[] = [...temps];
var a4: XY[] = [...['y']];
var a5: [XY] = [...['y']];
var a6: [XY] = [...temps];
var a7: XY[] = [...temps, ...temps];
var a8: XY[] = [...['y'], ...['x']];
var a9: XY[] = ['x', ...['y']];
interface MyArr extends Array<XY> {}
var b1: MyArr = [...['y']];
var b2: MyArr = [...temps];

// A concrete type is not assignable to `(M & O)[K]` with a generic mapped `M` and a
// generic key.
type Errors<T> = { [P in keyof T]: string | undefined } & { all: string | undefined };
function foo<T>(k: keyof T) {
    let obj!: Errors<T>;
    obj[k] = undefined;
    const v: string | undefined = obj[k];
}
