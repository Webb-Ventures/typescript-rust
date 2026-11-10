interface Err<T> {
    e: T;
}
interface Opts<A, B> {
    a?: A;
    b?: B;
}
export declare function f<T>(): {
    g: (path: string, opts?: Opts<unknown, Err<T>>) => Opts<unknown, Err<T>>;
    h: (opts: Err<T>) => void;
};
export {};
