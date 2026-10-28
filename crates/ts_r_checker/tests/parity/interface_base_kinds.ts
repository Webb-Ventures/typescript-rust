type U = { a: number } | { b: string };
interface I1 extends U { x: string }
interface I2<T> extends T { x: string }
interface I3<T extends object> extends T { x: string }
interface I4<T extends { a: number }> extends T { x: string }
type Prim = string;
interface I5 extends Prim {}
type RemapRecord<K extends keyof any, V> = { [_ in never as K]: V };
interface I6<K extends keyof any, V> extends RemapRecord<K, V> {}
interface I7<T> extends Omit<T, "a"> {}
interface I8 extends Omit<{ a: 1; b: 2 }, "a"> {}
interface I9 extends Partial<{ a: 1 }>, Readonly<{ b: 2 }> {}
interface I10<T> extends Partial<T> {}
interface I11<T> extends Array<T> {}
interface I12 extends Missing {}
interface I13<T> extends Record<string, T> {}
interface I14 extends object {}
type Inter<T> = { a: 1 } & T;
interface I15<T> extends Inter<T> {}
interface I16<T extends {}> extends Inter<T> {}
type T1 = { a: number };
type T2 = T1 & { b: number };
interface J2 extends T2 { b: string }
declare class Foo { x: string }
declare class Bar { y: string }
type Wrapper = Foo & Bar;
class Baz implements Wrapper { x: number = 1; y: string = "" }
interface J3<T extends object> extends T { x: string }
function useJ3<T extends { a: number }>(j: J3<T>) { return j.x; }
