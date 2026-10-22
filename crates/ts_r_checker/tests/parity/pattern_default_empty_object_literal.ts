try {
    var { e = 1 } = { };
    var { g = 1 } = { g: 2 };
    var [d = 1] = [];
} catch {
}
const t1: string = e;
const t2: string = g;
e;
g;
d;
export {};
