// `typeof o !== "object"` removes the `object` keyword type (not `{}` or a function type).
declare var c: object;
if (typeof c !== "object") { const x: never = c; }
if (typeof c !== "function") { const x: never = c; }
declare var d: { a: 1 };
if (typeof d !== "object") { const x: never = d; }
if (typeof d !== "function") { const x: never = d; }
declare var e: () => void;
if (typeof e !== "object") { const x: never = e; }
if (typeof e !== "function") { const x: never = e; }
declare var f: {};
if (typeof f !== "object") { const x: never = f; }
