// A readonly array or tuple against a mutable one: TS4104 replaces the head (also an
// argument's), naming the alias when the source has one.
export {};
declare let ro2: readonly string[];
const d5: string[] = ro2;
type RO = readonly string[];
declare let ro: RO;
const d3: string[] = ro;
function f(x: string[]) {}
f(ro);
f(ro2);
