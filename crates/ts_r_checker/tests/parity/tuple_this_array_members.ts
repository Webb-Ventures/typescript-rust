// A tuple's array methods see the tuple as `this` (`sort(): this` returns the tuple), so
// `[s]` is assignable to an array type with a required `0` (a match array).
interface MatchLike extends Array<string> { 0: string; index?: number; input?: string }
declare function f(m: MatchLike): void;
declare function g(m: RegExpMatchArray): void;
declare const s: string;
f([s]);
g([s]);
const take = ([a, b = '']: RegExpMatchArray) => a + b;
take([s]);
const pair: [string, number] = ["a", 1];
const sorted: [string, number] = pair.sort();
const reversed: [number, string] = pair.reverse();
const filled: [string, string] = ["a", "b"].fill("c");
