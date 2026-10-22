// A widened object literal that gains a sibling's missing property lists it where the
// property was first written; the stand-in for a name is shared, so later ones keep it.
var v = [{ x: 1, z: true }, { x: 2, y: "" }][0]
var v: {}
var w = [{ y: 1, z: 2 }, {}][0]
var w: {}
export {}
