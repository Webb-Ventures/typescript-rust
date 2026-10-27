"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
const react_1 = require("react");
const jsx_runtime_1 = require("react/jsx-runtime");
// @module: commonjs
// @target: es2015
// @jsx: react-jsx
// The require of `createElement` (a key after a spread) comes before the runtime's.
const props = { answer: 42 };
const a = (0, jsx_runtime_1.jsx)("div", Object.assign({}, props, { children: "text" }), "foo");
const b = (0, react_1.createElement)("div", Object.assign({}, props, { key: "bar" }), "text");
