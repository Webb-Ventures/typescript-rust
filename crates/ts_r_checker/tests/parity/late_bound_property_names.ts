// Late-bound property names are written as declared ([c0]); computed string literal names print quoted.
export {};
const c0 = "1";
const k = { name: "n" } as const;
interface T1 { [c0]: number; [k.name]: string }
interface T2 { [c0]: string; [k.name]: string }
interface T3 { [k.name]: number }
declare let t1: T1;
declare let t2: T2;
declare let t3: T3;
t1 = t2;
t3 = t1;
const t4: T1 = { [c0]: 1 };
