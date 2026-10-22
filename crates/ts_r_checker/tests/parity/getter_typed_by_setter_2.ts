class C {
    set X(x: number) { }
    get X() {
        return "string";
    }
    get Y() { return 1; }
    set Y(y: string) { }
}
const n: string = new C().X;
const m: number = new C().Y;
const o = {
    set X(x: number) { },
    get X() { return "s"; },
};
class D {
    set x(value: Array<[string, { x?: number, y?: number }]>) { }
    get x() {
        return [
            ['a', { x: 1 }],
            ['b', { y: 2 }]
        ];
    }
    set f(value: (a: string) => void) { }
    get f() { return a => { const n: number = a; }; }
}
export {};
