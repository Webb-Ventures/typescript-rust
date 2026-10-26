// The type implied by an object binding pattern lists its members in name order.
function f({ b, a, c = 1 }, [y, x]) { }
const n: string = f;
const dyn = "data-dyn";
const g = ({ "data-testid": id, [dyn]: d, z = "" }) => 0;
const m: string = g;
