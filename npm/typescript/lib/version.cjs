// `require("typescript")` through @ts-r.dev/typescript: TypeScript 7's main entry only exports its
// version, so this reports the real TypeScript's version (tools gate on it), or, when it isn't
// installed, the TypeScript version this package was built against.
'use strict';

const { OWN, findRealTypeScript } = require('./real.cjs');

const real = findRealTypeScript();
const version = real ? real.version : OWN['ts-r'].typescript;
const [major, minor] = version.split('.');

exports.version = version;
exports.versionMajorMinor = `${major}.${minor}`;
