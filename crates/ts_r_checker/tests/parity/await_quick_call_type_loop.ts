// A variable initialized by awaiting a call of a function with one non-generic signature
// takes its awaited return type without checking the arguments, so a loop assigning from it
// is no cycle.
declare let cond: boolean;
declare function asNumber(x: string | number): Promise<number>;
declare function asNumberS(x: string | number): number;

async function h3() {
    let x: string | number | boolean;
    x = "0";
    while (cond) {
        let y = await asNumber(x);
        x = y + 1;
        const s: string = x;
    }
}
function h4() {
    let x: string | number | boolean;
    x = "0";
    while (cond) {
        let y = asNumberS(x);
        x = y + 1;
        x;
    }
}
async function h5() {
    let x: string | number | boolean;
    x = "0";
    while (cond) {
        let y = (await asNumber(x));
        x = y + 1;
        x;
    }
}
export {};
