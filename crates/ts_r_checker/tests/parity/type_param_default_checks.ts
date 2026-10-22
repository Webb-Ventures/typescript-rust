// A type parameter's default must satisfy its constraint, instantiated with the parameter
// as the default: TS2344 at the default.
declare function f03<T extends string = number>(): void;
declare function f04<T extends string, U extends number = T>(): void;
declare function f05<T, U extends number = T>(): void;
declare function f06<T, U extends T = number>(): void;
interface i05<T extends string = number> { }
interface i08<T, U extends T = number> { }
type A1<T extends { a: number } = { a: string }> = T;
class C1<T extends C1 = any> {}
class C2<T extends C2<any> = any> {}
interface Comparable<T> { compare(other: T): number }
declare function ok1<T extends string = "a">(): void;
declare function ok2<T extends unknown[] = []>(): void;
declare function ok3<T extends Comparable<T> = Comparable<any>>(): void;
declare function ok4<O, K extends keyof O = keyof O>(): void;
type ok5<T extends readonly unknown[] = readonly string[]> = T;
interface SelfReference<T = SelfReference> {}
type Alias<T = Alias> = { t: T };
declare function g<T = T>(): void;
interface Ok<T = Ok<number>> { t: T }
