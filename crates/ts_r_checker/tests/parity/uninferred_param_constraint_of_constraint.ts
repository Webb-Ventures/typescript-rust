class A { foo(): string { return ''; } }
declare var a: { <U extends T, T extends A>(): U; }
var r3: string = a().notHere();
declare var b: { <T extends A, U extends T>(): U; }
var r4: string = b().notHere();
