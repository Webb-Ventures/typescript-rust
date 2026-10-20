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
// @target: es2022
// @jsx: react
// @module: commonjs
const React = __importStar(require("react"));
const comp_1 = require("./comp");
exports.a = React.createElement("div", { className: "x", ...p, key: "k" },
    "  hello & ",
    "{",
    " ",
    name,
    "world   ",
    React.createElement("br", null));
exports.b = React.createElement(React.Fragment, null,
    React.createElement(comp_1.Comp.Sub, { a: true, b: 1, c: "q\"", "d-e": "1", ...p }),
    "text on two lines",
    ...items);
exports.c = React.createElement(comp_1.Other, { key: name, ...p },
    React.createElement("span", null, "only"));
exports.d = React.createElement("svg:rect", { "xlink:href": "#x" });
exports.e = React.createElement("div", { ...p, key: "after" });
exports.f = React.createElement(comp_1.Comp, { prop: React.createElement("i", null, "n") });
exports.g = React.createElement("div", null, items.map(i => React.createElement("b", { key: i }, i)));
exports.h = React.createElement("div", null);
