// A namespace body is its own flow container: outer variables are assumed assigned.
namespace a {
    var x: number;
    namespace b {
        var y = x;
    }
    var z: number;
    var w = z;
}
namespace A {
    export interface X { s: string }
    export var X: X;
}
namespace B {
    import Z = A.X;
    var v: Z = Z;
}

// `for ([a, b] of xs)` assigns its pattern's targets; defaults must fit them.
declare var console: { log(msg: any): void };
type Robot = [number, string, string];
declare const robots: Robot[];
let nameA: string, skillA: string;
for ([, nameA] of robots) {
    console.log(nameA);
}
for ([, nameA = 1, skillA] of robots) {
    console.log(skillA);
}
let k: string, v: boolean;
for ([k = false, v = ""] of new Map<string, boolean>()) {
    k;
}
let s1: string;
for ({ s1 = 5 } of [{ s1: "a" }]) {
}
(function () {
    let y2: string, y3: { x: string };
    ({ y2 = 5, y3 = { x: 1 } } = {});
});

export {};
