{
    using a = null,
          {b} = null,
          c = null;
}
{
    const {d} = null;
    let {e} = undefined;
    using {f} = undefined;
    await using {g} = null;
    using [h] = null;
}
// @filename: tsconfig.json
{ "compilerOptions": { "strict": false, "target": "esnext", "lib": ["esnext"], "module": "esnext", "moduleDetection": "force", "noEmit": true } }
