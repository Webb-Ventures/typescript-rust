class K {
    private priv = 1;
    protected prot = 2;
    m() {
        const { priv, prot } = new K();
        return priv + prot;
    }
}
class D extends K {
    m2() {
        const { priv: a } = this;
        const { prot: b } = this;
        return [a, b];
    }
}
const k = new K();
const { priv } = k;
const { ["prot"]: p2 } = k;
const name = "priv";
const { [name]: p3 } = k;
let x = 0;
({ priv: x } = k);
({ [name]: x } = k);
function f({ priv, prot }: K) { return [priv, prot]; }
export { priv, p2, p3, x, f, D };
