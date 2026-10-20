// @target: es2022
// @preserveConstEnums: true
const enum CE { X = 1, Y = X + 1 }
let a = CE.Y;
export {};
