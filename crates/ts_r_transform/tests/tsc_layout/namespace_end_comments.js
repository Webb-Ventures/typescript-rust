"use strict";
var hello;
(function (hello) {
    var hi;
    (function (hi) {
        var world;
        (function (world) {
            function foo() { }
            // TODO, blah
        })(world = hi.world || (hi.world = {}));
    })(hi = hello.hi || (hello.hi = {}));
})(hello || (hello = {}));
