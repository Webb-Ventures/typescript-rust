// Recovery declares missing names as `(Missing)`, as in tsc (reservedWords3,
// parametersSyntaxErrorNoCrash2 baselines): an enum and a function named `(Missing)` conflict
// (TS2567), and two missing binding names in one parameter are duplicates (TS2300).
function f1(enum) {}
function f3(function) {}
function g(a, { return 'thing'; }) {}
