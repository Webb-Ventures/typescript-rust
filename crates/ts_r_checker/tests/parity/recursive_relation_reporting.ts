// While reporting, a pair already being compared is assumed related even though its failure
// is cached: the error names the property that really differs.
interface A1<T> {
    every(predicate: (value: number, array: this) => unknown): boolean;
    tag: "A";
}
interface B1<T> {
    every(predicate: (value: number, array: this) => unknown): boolean;
    tag: "B";
}
declare let a: A1<string>;
declare let b: B1<string>;
a = b;
interface A2 {
    every(predicate: (value: number, array: A2) => unknown): boolean;
    tag: "A";
}
interface B2 {
    every(predicate: (value: number, array: B2) => unknown): boolean;
    tag: "B";
}
declare let a2: A2;
declare let b2: B2;
a2 = b2;
declare let i8: Int8Array;
declare let u8: Uint8Array;
i8 = u8;
