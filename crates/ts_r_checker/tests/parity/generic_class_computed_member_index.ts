// A member with a computed key of a non-literal type implies an index signature. For a
// generic class or interface its type is `any`; a non-generic class and a type literal
// take the union of the member types. Inference from a derived class to its generic base
// therefore sees no garbage from the index.
declare const skey: string;
class G1<T> { value!: string; [skey]() { return 1; } }
class G2<T> { value!: T; [skey]!: number }
interface GI<T> { value: T; [skey]: number }
type GT<T> = { value: T; [skey]: number };
class N { value!: string; [skey]!: number }
class D extends G2<string> { other!: boolean }
declare const g1: G1<number>, g2: G2<number>, gi: GI<number>, gt: GT<number>, n: N, d: D;
const t1: boolean = g1["x"];
const t2: boolean = g2["x"];
const t3: boolean = gi["x"];
const t4: boolean = gt["x"];
const t5: boolean = n["x"];
const t6: boolean = d["x"];
const t7: { [k: string]: boolean } = g2;
class Base<T> {
  items: ((value: T) => void)[] = [];
  [skey]() { return this; }
}
class Derived<T> extends Base<T> {
  constructor(private initial: T) { super(); }
}
declare function make<T>(f: () => Base<T>): T;
declare const derived: () => Derived<string>;
const r: number = make(derived);
