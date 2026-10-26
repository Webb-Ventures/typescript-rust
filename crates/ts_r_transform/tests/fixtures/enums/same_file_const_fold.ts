// @target: esnext
const base = 4;
const label = "x";
const neg = -base;
enum E {
    A = base,
    B = base * 2,
    C = label,
    D = neg,
    F = later,
}
const later = 9;
