// Narrowing reaches function expressions, arrows, object literal members and members of
// class expressions; not constructors, members of class declarations, nor any `var`.
declare const k: string | number;
if (typeof k === "string") {
    class D {
        constructor() { let a: string = k; }
        m() { let a: string = k; }
        get g() { let a: string = k; return 1; }
        p = () => { let a: string = k; };
        q = k.length;
        static s = k.length;
        static { let a: string = k; }
    }
    const E = class {
        constructor() { let a: string = k; }
        m() { let a: string = k; }
        p = () => { let a: string = k; };
        q = k.length;
    };
    const o = { m() { let a: string = k; }, get g() { let a: string = k; return 1; } };
    function h() { let a: string = k; }
}
declare var x: string | number;
declare let y: string | number;
let z: string | number = Math.random() ? "" : 1;
var w: string | number = Math.random() ? "" : 1;
if (typeof x === "string" && typeof y === "string" && typeof z === "string" && typeof w === "string") {
    const f = () => { let a: string = x; let b: string = y; let c: string = z; let d: string = w; };
    let n = class {
        constructor() { let a: string = x; let b: string = y; let c: string = z; let d: string = w; }
        m() { let a: string = x; let b: string = y; let c: string = z; let d: string = w; }
    };
}
function g(p: string | number) {
    var v: string | number = p;
    if (typeof p === "string" && typeof v === "string") {
        const f = () => { let a: string = p; let b: string = v; };
    }
}
