f(/*a1*/ x /*a2*/, /*a3*/ y /*a4*/);
f(/*b1*/);
f( /*b2*/ );
f(x, // c1
  y);
f(
  // c2
  x,
  // c3
  y
);
f(/*d1*/
  x);
var o = {
    a: 1, // e1
    b: 2 // e2
    , c: 3 /* e3 */
    // e4
};
var o2 = { /*g1*/ a: 1 /*g2*/ };
var a = [/*h1*/ 1 /*h2*/, /*h3*/ 2 /*h4*/];
var a2 = [
    1, // h5
    // h6
    2
];
var s = /*i1*/ x /*i2*/ + /*i3*/ y /*i4*/;
var u = x ? /*j1*/ y : /*j2*/ z;
var v = (/*k1*/ x /*k2*/);
var w = x./*l1*/y;
var w2 = x /*l2*/ .y;
var c = function /*m1*/ (/*m2*/ p /*m3*/) /*m4*/ { /*m5*/ };
var d = (/*n1*/ p /*n2*/) => /*n3*/ p;
var e = new /*o1*/ f(/*o2*/);
var g = typeof /*p1*/ x;
var h = !/*p2*/x;
var k = x[/*q1*/ 0 /*q2*/];
var r = x /*s4*/;
var r2 = x; /*s5*/
let /*t1*/ v1 /*t2*/ = /*t3*/ 1 /*t4*/, /*t5*/ v2 = 2;
class K {
    /*v4*/ m(/*v5*/) /*v6*/ { }
    p /*v7*/ = /*v8*/ 1;
}
var x1 = 1 // asi
var g2 = x => { x.key; /* kept */ };
