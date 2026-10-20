// @target: es2022
// @module: esnext
// An enum merged across blocks of one namespace: each block declares it, members of earlier
// blocks fold (before outer names), and `N.E.A` folds (esbuild oracle: TSSiblingEnum).
export var t;
(function (t) {
    t[t["y"] = 0] = "y";
    t[t["yy"] = 0] = "yy";
})(t || (t = {}));
(function (t) {
    t[t["z"] = 1] = "z";
})(t || (t = {}));
export var foo;
(function (foo) {
    let x;
    (function (x) {
        x[x["y"] = 0] = "y";
        x[x["yy"] = 0] = "yy";
    })(x = foo.x || (foo.x = {}));
})(foo || (foo = {}));
(function (foo) {
    let x;
    (function (x) {
        x[x["z"] = 1] = "z";
    })(x = foo.x || (foo.x = {}));
})(foo || (foo = {}));
(function (foo) {
    var x;
    (function (x) {
        console.log(y, z);
    })(x = foo.x || (foo.x = {}));
})(foo || (foo = {}));
export var n;
(function (n) {
    let a;
    (function (a) {
        a[a["b"] = 100] = "b";
    })(a = n.a || (n.a = {}));
})(n || (n = {}));
(function (n) {
    let x;
    (function (x) {
        x[x["c"] = 100] = "c";
        x[x["d"] = 200] = "d";
        x[x["e"] = 40000] = "e";
        x[x["f"] = 10000] = "f";
    })(x = n.x || (n.x = {}));
})(n || (n = {}));
(function (n) {
    let x;
    (function (x) {
        x[x["g"] = 625] = "g";
    })(x = n.x || (n.x = {}));
    console.log(n.a.b, n.a.b, x.g, n.x.g);
})(n || (n = {}));
export var p;
(function (p) {
    var q;
    (function (q) {
        q.v = 1;
    })(q = p.q || (p.q = {}));
})(p || (p = {}));
(function (p) {
    var q;
    (function (q) {
        console.log(q.v);
    })(q = p.q || (p.q = {}));
})(p || (p = {}));
