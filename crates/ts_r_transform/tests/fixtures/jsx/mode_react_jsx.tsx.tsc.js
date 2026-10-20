import { Fragment as _Fragment, jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { createElement as _createElement } from "react";
// @target: es2022
// @jsx: react-jsx
import * as React from "react";
import { Comp, Other } from "./comp";
export const a = _createElement("div", { className: "x", ...p, key: "k" },
    "  hello & ",
    "{",
    " ",
    name,
    "world   ",
    _jsx("br", {}));
export const b = _jsxs(_Fragment, { children: [_jsx(Comp.Sub, { a: true, b: 1, c: "q\"", "d-e": "1", ...p }), "text on two lines", ...items] });
export const c = _jsx(Other, { ...p, children: _jsx("span", { children: "only" }) }, name);
export const d = _jsx("svg:rect", { "xlink:href": "#x" });
export const e = _createElement("div", { ...p, key: "after" });
export const f = _jsx(Comp, { prop: _jsx("i", { children: "n" }) });
export const g = _jsx("div", { children: items.map(i => _jsx("b", { children: i }, i)) });
export const h = _jsx("div", {});
