// A class merged with function declarations must be ambient.
class Foo { constructor(x: any) { } }
function Foo(): void;

function bar(): string;
class bar { }

function baz() { }
namespace baz { export function x(): any { } }
class baz { }

declare function qux(): void;
declare class qux { }

declare class quux { }
function quux() { }
export {};
