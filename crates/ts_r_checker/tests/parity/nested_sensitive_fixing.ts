// Context-sensitive functions nested in an array or object literal argument fix the type
// parameters their parameters mention (to `unknown` with no inferences), so overloads
// of their contextual types resolve against the fixed instance.
interface Callback<T> {
    (error: null, result: T): unknown
    (error: Error, result: null): unknown
}
interface Task<T> {
    (callback: Callback<T>): unknown
}
declare function series<T>(tasks: Task<T>[], callback: Callback<T[]>): void;
series([], (error, results) => {
    const r: string = results;
});
series([cb => 0], (error, results) => {
    const r: string = results;
});
series([cb => cb(null, 1)], (error, results) => {
    const r: string = results;
});
declare function s5<T>(tasks: Task<T>[]): void;
s5([cb => cb(null, 1)]);
declare function callIt<T>(obj: {
    produce: (n: number) => T,
    consume: (x: T) => void
}): void;
callIt({
    produce: _a => 0,
    consume: n => n.toFixed(),
});
export {};
