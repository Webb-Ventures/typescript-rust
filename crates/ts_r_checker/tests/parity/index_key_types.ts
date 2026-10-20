type S = string;
type U = string | number;
type L = 'a' | 'b';
enum E { A, B }
enum SE { A = "a" }
interface Purple { }
class C { }
type Br = string & { __b: 1 };
type TL = `x${string}`;
interface I1 { [k: S]: any }
interface I2 { [k: U]: any }
interface I3 { [k: L]: any }
interface I4 { [k: E]: any }
interface I5 { [k: Purple]: any }
interface I6 { [k: C]: any }
interface I7 { [k: Br]: any }
interface I8 { [k: TL]: any }
interface I9<T> { [k: T]: any }
interface I10<T extends string> { [k: T]: any }
interface I11 { [k: NotFound]: any }
interface I12 { [k: string | boolean]: any }
interface I13 { [k: RegExp]: any }
interface I14 { [k: SE]: any }
interface I15 { [k: 'x']: any }
interface I17 { [k: string | 'a']: any }
interface I18 { [k: Uppercase<string>]: any }
interface I19 { [k: keyof Purple]: any }
type M = { [k: S]: any };
interface A2 { [k: true]: any }
interface B2 { [k: boolean]: any }
type Bo2 = boolean;
interface C2 { [k: Bo2]: any }
interface D2 { [k: "a" | true]: any }
interface E2 { [k: 1n]: any }
interface A3 { [k: "a" | RegExp]: any }
interface B3 { [k: 1 | object]: any }
enum E3 { X }
interface C3 { [k: E3 | Date]: any }
interface D3 { [k: string | 1n]: any }
