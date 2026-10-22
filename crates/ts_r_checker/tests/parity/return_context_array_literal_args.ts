// An array literal argument is contextually typed by what the call's contextual type
// implies for its parameter's type parameter: a tuple stays a tuple, and an array reverse-
// mapped through a homomorphic mapped return type keeps its element type.
interface OK<T> { kind: "OK"; value: T; }
declare function ok<T>(value: T): OK<T>;
let result: OK<[string, number]> = ok(["hello", 12]);
declare function id<T>(value: T): T;
let pair: [string, boolean] = id(["a", true]);
let arr: (string | number)[] = id(["a", 1]);
let bad: OK<[string, number]> = ok(["hello", "x"]);

type Player = { name: string; position: "STRIKER" | "GOALKEEPER" };
type F = () => Promise<Array<Player>>;
const f1: F = () => {
    return Promise.all([{ name: "D", position: "GOALKEEPER" }]);
};
const p2: Promise<Player[]> = Promise.all([{ name: "D", position: "GOALKEEPER" }]);
declare function all<T extends readonly unknown[] | []>(values: T): { -readonly [P in keyof T]: Awaited<T[P]> };
const p3: Player[] = all([{ name: "D", position: "GOALKEEPER" }]);
const p4: Player[] = all([{ name: "D", position: "KEEPER" }]);
