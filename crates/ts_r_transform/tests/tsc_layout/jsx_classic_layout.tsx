// @jsx: react
// @target: es2022
declare var React: any;
declare var x: any;
var a = <div x='1' y="2" z='a&amp;b' w='é' />;
var b = <p><b /></p>;
var c = <p>{/* c */}<b /></p>;
var d = <p> <b /></p>;
var e = <p>{x}{x}</p>;
var f = <p>x{}</p>;
var g = <><span>1</span><><span>2</span><span>3</span></></>;
