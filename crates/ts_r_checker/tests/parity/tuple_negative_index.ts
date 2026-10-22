// A negative literal index into a tuple without a rest element is TS2514, in expressions
// and in indexed access types; tuples with a rest element and arrays accept it.
declare const t: [number, string];
const a = t[-1];
type B = [number, string][-1];
declare const r: [number, ...string[]];
const c = r[-1];
declare const arr: number[];
const d = arr[-1];
