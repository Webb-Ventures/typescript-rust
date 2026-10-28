// @module: commonjs
// @target: es2015
// @jsx: react-jsx
// The require of `createElement` (a key after a spread) comes before the runtime's.
const props = { answer: 42 }
const a = <div key="foo" {...props}>text</div>;
const b = <div {...props} key="bar">text</div>;
export {};
