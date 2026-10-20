"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.h = exports.g = exports.f = exports.e = exports.d = exports.c = exports.b = exports.a = void 0;
const jsx_runtime_1 = require("react/jsx-runtime");
const react_1 = require("react");
// @target: es2022
// @jsx: react-jsx
// @module: commonjs
const React = __importStar(require("react"));
const comp_1 = require("./comp");
exports.a = (0, react_1.createElement)("div", { className: "x", ...p, key: "k" },
    "  hello & ",
    "{",
    " ",
    name,
    "world   ",
    (0, jsx_runtime_1.jsx)("br", {}));
exports.b = (0, jsx_runtime_1.jsxs)(jsx_runtime_1.Fragment, { children: [(0, jsx_runtime_1.jsx)(comp_1.Comp.Sub, { a: true, b: 1, c: "q\"", "d-e": "1", ...p }), "text on two lines", ...items] });
exports.c = (0, jsx_runtime_1.jsx)(comp_1.Other, { ...p, children: (0, jsx_runtime_1.jsx)("span", { children: "only" }) }, name);
exports.d = (0, jsx_runtime_1.jsx)("svg:rect", { "xlink:href": "#x" });
exports.e = (0, react_1.createElement)("div", { ...p, key: "after" });
exports.f = (0, jsx_runtime_1.jsx)(comp_1.Comp, { prop: (0, jsx_runtime_1.jsx)("i", { children: "n" }) });
exports.g = (0, jsx_runtime_1.jsx)("div", { children: items.map(i => (0, jsx_runtime_1.jsx)("b", { children: i }, i)) });
exports.h = (0, jsx_runtime_1.jsx)("div", {});
