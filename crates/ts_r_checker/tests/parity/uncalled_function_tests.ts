declare const b: boolean;
function t1(req: () => void) { while (req) { break; } }
function t2(req: () => void) { for (; req;) { break; } }
function t3(req: () => void) { if (!req) {} }
function t4(req: () => void) { req ? 1 : 2; }
function t5(req: () => void) { req ? req() : 2; }
function t6(req: () => void) { req ? 1 : req(); }
function t7(req: () => void) { if (b) {} else if (req) {} }
function t8(req: () => void) { if (req) {} else { req(); } }
function t9(req: () => void) { if (req) { const q = req; } }
function t10(req: () => void) { if (req) { req.call(null); } }
function t11(req: () => void) { const w = !!req; }
function t12(req: () => void) { if (req && b) {} }
function t13(req: () => void) { if (b && req) {} }
function t14(req: () => void) { const v = b && req; }
function t15(req: () => void) { const v = b && req && 1; }
function t16(req: () => void) { if (req) { return; } req(); }
function t17(req: () => void) { (req) ? 1 : 2; }
function t18(req: () => void) { if (b ? req : req) {} }
function t19(req: () => void) { if (req as any) {} }
function t20(req: () => void) { if (req!) {} }
function t21(req: () => void) { if (b || req) {} }
function t22(req: () => void, r2: () => void) { if (req) { r2(); } }
function t23(req: () => void) { if (req) { function inner() { req(); } } }
function t24(req: () => void) { if (req) { let req = 1; req; } }
function t25(req: () => void) { const o = { req }; if (o.req) { o.req(); } }
function t26(req: () => void) { const o = { req }; if (o.req) { o["req"](); } }
function t27(req: () => void) { const o = { req }; if (o.req) { req(); } }
function t28(req: () => void) { const o = { req }; if (o["req"]) { } }
function t29(req: () => void) { if (req) { req; } }
function t30(req: () => void) { if (req) { typeof req; } }
function t31(req: () => void) { if (req) { req = () => {}; } }
export {};
