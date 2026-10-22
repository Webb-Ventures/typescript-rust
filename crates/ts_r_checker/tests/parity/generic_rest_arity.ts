// A generic rest parameter instantiated by inference takes the arguments of its
// instance: too few or too many is TS2554 for that instance.
declare function call<TS extends unknown[]>(
    handler: (...args: TS) => void,
    ...args: TS): void;
call((x: number, y: number) => x + y);
call((x: number, y: number) => x + y, 1, 2, 3, 4, 5, 6, 7);
call((x: number, y: number) => x + y, 1, 2);
declare function call2<Fn extends (...args: any[]) => any>(
    fn: Fn,
    ...args: Parameters<Fn>
): any;
call2(function* (a: 'a') { });
call2(function* (a: 'a') { }, 'a');
function bind(f: (a: number, b: string) => void) {
    f.call(undefined, 1);
    f.call(undefined, 1, "", 3);
}
export {};
