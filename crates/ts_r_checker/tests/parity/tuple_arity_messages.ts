// Tuple arity messages: an optional source element for a required target element is
// TS2623 at its position; a rest element before required ones needs only those.
export {};
type T1 = [number, string, boolean];
type T2 = [number, string, boolean?];
declare let t1: T1;
declare let t2: T2;
declare let u1: [number, string, boolean];
declare let u2: [number, string, boolean?];
t1 = t2;
u1 = u2;
t1 = u2;
u1 = t2;
type Funcs = [...((arg: number) => void)[], (arg: string) => void];
const a0: Funcs = [];
const a1: [...((arg: number) => void)[], (arg: string) => void] = [];
type F2 = [...number[], string];
const a2: F2 = [];
const a3: number = a2;
const a4: number = null! as Funcs;
