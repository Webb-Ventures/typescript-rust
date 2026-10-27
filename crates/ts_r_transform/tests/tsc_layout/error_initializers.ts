// @target: es2022
// Initializers on a rest element and a catch variable (errors) are kept.
declare var a: any;
var [...x = a] = a;
function b(...[...foo = []]) { }
var {...r = 1} = a;
try { } catch (e = 1) { }
