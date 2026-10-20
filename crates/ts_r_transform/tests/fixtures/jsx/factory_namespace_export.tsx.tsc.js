"use strict";
// @target: es2022
// @jsx: react
// The classic factory's root names an export of another block of the enclosing namespace:
// `y.React.createElement` (esbuild oracle: TSEnumJSX).
var x;
(function (x) {
    var y;
    (function (y) {
        let React;
        (function (React) {
            React["Fragment"] = "div";
        })(React = y.React || (y.React = {}));
    })(y = x.y || (x.y = {}));
})(x || (x = {}));
(function (x) {
    var y;
    (function (y) {
        console.log(y.React.createElement(y.React.Fragment, null, "test"));
    })(y = x.y || (x.y = {}));
})(x || (x = {}));
