// `static prototype` merges with the class's own prototype symbol: no implicit-any error.
class StaticProto {
    static prototype;
}

// `for (var in X)` with an empty declaration list still checks the right operand.
declare let notDeclaredHere: number;
for (var in missingName) { }

// An unresolved `import x = N.M` still reports the missing leftmost name when used.
import Alias = NoSuchNamespace.Member;
const useAlias = Alias;

// `class extends null` with base types from a merged interface compares the static
// side with `null`.
class Base0 { }
interface Nulled extends Base0 { }
class Nulled extends null { }

// Mixins: the constructor must take a single rest parameter of type any[].
type Ctor<T = {}> = new (...args: any[]) => T;
function WrongMixin<T extends Ctor>(B: T) {
    return class extends B {
        constructor(x: number) {
            super();
        }
    };
}

// Mixins over an abstract constructor type must be abstract themselves.
type AbstractCtor<T = {}> = abstract new (...args: any[]) => T;
function AbstractMixin<T extends AbstractCtor>(B: T) {
    class Mixed extends B { }
    abstract class MixedOk extends B { }
    return [Mixed, MixedOk];
}
