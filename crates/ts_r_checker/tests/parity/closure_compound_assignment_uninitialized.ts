function rw() {
    let i: number;
    function inside() {
        i++;
        String(i);
    }
    inside();
}

function createBinder() {
    var file: string;
    function bindSourceFile(f: string) {
        file = f;
        file.toString();
    }
}

function flags() {
    let n: number;
    function enable() {
        n |= 1;
        n.toString();
    }
    let m: number;
    function logical() {
        m ??= 1;
        m.toString();
    }
    let k: number;
    function paren() {
        (k)--;
        k.toFixed();
    }
    let both: number;
    function mixed() {
        both += 1;
        both = 2;
        both.toFixed();
    }
}
export {};
