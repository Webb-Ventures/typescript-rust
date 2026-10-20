"use strict";
// @target: es2022
// @module: commonjs
const fs = require("fs");
class C {
    f = fs;
}
module.exports = C;
