"use strict";
var a = React.createElement("div", { x: '1', y: "2", z: 'a&b', w: '\u00E9' });
var b = React.createElement("p", null,
    React.createElement("b", null));
var c = React.createElement("p", null,
    React.createElement("b", null));
var d = React.createElement("p", null,
    " ",
    React.createElement("b", null));
var e = React.createElement("p", null,
    x,
    x);
var f = React.createElement("p", null, "x");
var g = React.createElement(React.Fragment, null,
    React.createElement("span", null, "1"),
    React.createElement(React.Fragment, null,
        React.createElement("span", null, "2"),
        React.createElement("span", null, "3")));
