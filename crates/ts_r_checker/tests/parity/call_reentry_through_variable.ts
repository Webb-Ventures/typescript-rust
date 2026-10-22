declare function useState<S>(initialState: (() => S)): [S, (s: S) => void];
type Data = { value: number; foo: (arg: any) => void; bar: (arg: any) => void; };
export function A() {
    const [state, setState] = useState<Data>(() => ({
        value: "a",
        foo: (arg) => setState(arg),
        bar: (arg) => setState(arg),
    }));
}
type D2 = Readonly<{ value: number; foo: (arg: any) => void; }>;
export function B() {
    const [state, setState] = useState<D2>(() => ({
        value: "b",
        foo: (arg) => setState(arg),
    }));
}
