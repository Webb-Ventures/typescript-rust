// Function declarations bind first; var hoists to the function scope; let/const/class are
// block scoped; catch bindings live in the catch scope.
f();
function f() { return g; var g = 1; }
{
    let a = 1;
    const b = a;
    class K {}
    var hoisted = b;
}
try { } catch (e) { let inner = e; }
for (let i = 0; i < 2; i++) { var j = i; }
function outer<T>(p: T, { q, r = q }: { q: number; r?: number }) {
    type T2 = T;
    return arguments.length + p + r;
}
