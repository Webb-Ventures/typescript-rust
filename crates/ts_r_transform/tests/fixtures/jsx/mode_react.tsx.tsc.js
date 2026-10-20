// @target: es2022
// @jsx: react
import * as React from "react";
import { Comp, Other } from "./comp";
export const a = React.createElement("div", { className: "x", ...p, key: "k" },
    "  hello & ",
    "{",
    " ",
    name,
    "world   ",
    React.createElement("br", null));
export const b = React.createElement(React.Fragment, null,
    React.createElement(Comp.Sub, { a: true, b: 1, c: "q\"", "d-e": "1", ...p }),
    "text on two lines",
    ...items);
export const c = React.createElement(Other, { key: name, ...p },
    React.createElement("span", null, "only"));
export const d = React.createElement("svg:rect", { "xlink:href": "#x" });
export const e = React.createElement("div", { ...p, key: "after" });
export const f = React.createElement(Comp, { prop: React.createElement("i", null, "n") });
export const g = React.createElement("div", null, items.map(i => React.createElement("b", { key: i }, i)));
export const h = React.createElement("div", null);
