"use strict";
// @target: es2015
class c4 {
    foo() { }
}
(function (c4) {
    c4[c4["One"] = 0] = "One";
})(c4 || (c4 = {}));
var x;
(function (x) {
    x.y = 123;
})(x || (x = {}));
(function (x) {
    x[x["z"] = 1] = "z";
})(x || (x = {}));
function f() { }
(function (f) {
    f.v = 1;
})(f || (f = {}));
var console;
(function (console) {
    console.log2 = 1;
})(console || (console = {}));
var K;
(function (K) {
    K.b = 1;
})(K || (K = {}));
var T;
(function (T) {
    var x = 2;
    (function (x) {
        class Bar {
        }
        x.Bar = Bar;
    })(x || (x = {}));
    let z;
    (function (z) {
        var t;
    })(z || (z = {}));
    var z;
    let y;
    (function (y) {
        var b;
    })(y || (y = {}));
    class y {
    }
    var w;
})(T || (T = {}));
