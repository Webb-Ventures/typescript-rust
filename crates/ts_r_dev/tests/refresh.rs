//! React Fast Refresh in headless Chrome, one module per way of defining a component.
//! Each component counts clicks; after one click, its module is edited and the test checks
//! the new text arrived without a page reload, with the count kept (state preserved) or
//! reset (remounted) as React's documented Fast Refresh rules say:
//!
//! - function components keep state: `export default function Named`, components inside
//!   `memo`/`forwardRef` (also anonymous ones, and nested), and HOC-wrapped exports
//!   (`export default withBox(Inner)`);
//! - anonymous `export default function () {}` / `export default () => …` and class
//!   components remount (the update goes to the importer, which renders a new type);
//! - changing a component's hooks (here `useState`'s initial value) remounts it, and so
//!   does `// @refresh reset`.
//!
//! Needs a directory with `react`, `react-dom` (and `scheduler`) and `react-refresh`
//! installed: `TS_R_REACT_ROOT=<dir with node_modules>`. Skipped with a message without it
//! or without Chrome.

mod support;

use std::path::PathBuf;
use std::time::Duration;

use support::{Chrome, DevProcess, find_chrome};
use ts_r_hmr::testutil::TempDir;

const T: Duration = Duration::from_secs(20);

/// `(file, button id, source with LABEL and INIT placeholders)`.
const CASES: &[(&str, &str, &str)] = &[
    (
        "named.jsx",
        "named",
        "import { useState } from 'react';\nexport default function Named() {\n  const [n, setN] = useState(INIT);\n  return <button id=\"named\" onClick={() => setN(n + 1)}>{'named LABEL ' + n}</button>;\n}\n",
    ),
    (
        "anon-fn.jsx",
        "anonfn",
        "import { useState } from 'react';\nexport default function () {\n  const [n, setN] = useState(INIT);\n  return <button id=\"anonfn\" onClick={() => setN(n + 1)}>{'anonfn LABEL ' + n}</button>;\n}\n",
    ),
    (
        "anon-arrow.jsx",
        "anonarrow",
        "import { useState } from 'react';\nexport default () => {\n  const [n, setN] = useState(INIT);\n  return <button id=\"anonarrow\" onClick={() => setN(n + 1)}>{'anonarrow LABEL ' + n}</button>;\n};\n",
    ),
    (
        "memo.jsx",
        "memo",
        "import { memo, useState } from 'react';\nexport default memo(() => {\n  const [n, setN] = useState(INIT);\n  return <button id=\"memo\" onClick={() => setN(n + 1)}>{'memo LABEL ' + n}</button>;\n});\n",
    ),
    (
        "forward-ref.jsx",
        "fwd",
        "import { forwardRef, useState } from 'react';\nexport const Fwd = forwardRef((props, ref) => {\n  const [n, setN] = useState(INIT);\n  return <button ref={ref} id=\"fwd\" onClick={() => setN(n + 1)}>{'fwd LABEL ' + n}</button>;\n});\n",
    ),
    (
        "memo-fwd.jsx",
        "memofwd",
        "import { memo, forwardRef, useState } from 'react';\nexport default memo(forwardRef(function (props, ref) {\n  const [n, setN] = useState(INIT);\n  return <button ref={ref} id=\"memofwd\" onClick={() => setN(n + 1)}>{'memofwd LABEL ' + n}</button>;\n}));\n",
    ),
    (
        "hoc.jsx",
        "hoc",
        "import { useState } from 'react';\nfunction withBox(C) {\n  return function WithBox(props) {\n    return <div className=\"box\"><C {...props} /></div>;\n  };\n}\nfunction Inner() {\n  const [n, setN] = useState(INIT);\n  return <button id=\"hoc\" onClick={() => setN(n + 1)}>{'hoc LABEL ' + n}</button>;\n}\nexport default withBox(Inner);\n",
    ),
    (
        "reset.jsx",
        "reset",
        "// @refresh reset\nimport { useState } from 'react';\nexport default function Reset() {\n  const [n, setN] = useState(INIT);\n  return <button id=\"reset\" onClick={() => setN(n + 1)}>{'reset LABEL ' + n}</button>;\n}\n",
    ),
    (
        "klass.jsx",
        "klass",
        "import { Component } from 'react';\nexport default class Klass extends Component {\n  state = { n: INIT };\n  render() {\n    const { n } = this.state;\n    return <button id=\"klass\" onClick={() => this.setState({ n: n + 1 })}>{'klass LABEL ' + n}</button>;\n  }\n}\n",
    ),
];

fn source(template: &str, label: &str, init: u32) -> String {
    template
        .replace("LABEL", label)
        .replace("INIT", &init.to_string())
}

#[test]
fn fast_refresh_keeps_or_resets_state_per_react_rules() {
    let Some(chrome_bin) = find_chrome() else {
        eprintln!("skipping: Chrome not found (set TS_R_CHROME)");
        return;
    };
    let Some(react_root) = std::env::var_os("TS_R_REACT_ROOT").map(PathBuf::from) else {
        eprintln!(
            "skipping: set TS_R_REACT_ROOT to a directory whose node_modules has react, react-dom and react-refresh"
        );
        return;
    };
    let modules = react_root.join("node_modules");
    assert!(
        modules.join("react-refresh").is_dir() && modules.join("react-dom").is_dir(),
        "{} lacks react-dom or react-refresh",
        modules.display()
    );
    let app = TempDir::new("e2e-refresh");
    let cache = TempDir::new("e2e-refresh-cache");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&modules, app.path().join("node_modules")).unwrap();
    app.write(
        "index.html",
        "<!doctype html><body><div id=\"root\"></div><script type=\"module\" src=\"/src/main.jsx\"></script></body>\n",
    );
    app.write(
        "src/main.jsx",
        "import { createRoot } from 'react-dom/client';\nimport App from './App.jsx';\nwindow.__marker ??= 'page-' + Math.random();\ncreateRoot(document.getElementById('root')).render(<App />);\n",
    );
    let mut imports = String::new();
    let mut tags = String::new();
    for (i, (file, _, _)) in CASES.iter().enumerate() {
        let name = if *file == "forward-ref.jsx" {
            imports.push_str(&format!(
                "import {{ Fwd as C{i} }} from './cases/{file}';\n"
            ));
            format!("C{i}")
        } else {
            imports.push_str(&format!("import C{i} from './cases/{file}';\n"));
            format!("C{i}")
        };
        tags.push_str(&format!("<{name} />"));
    }
    app.write(
        "src/App.jsx",
        &format!("{imports}export default function App() {{\n  return <div>{tags}</div>;\n}}\n"),
    );
    for (file, _, template) in CASES {
        app.write(&format!("src/cases/{file}"), &source(template, "v1", 0));
    }

    let dev = DevProcess::start(app.path(), cache.path(), &[]);
    let mut chrome = Chrome::launch(&chrome_bin);
    chrome.open(&dev.url);
    let cdp = &mut chrome.cdp;
    let text =
        |id: &str, t: &str| format!("document.getElementById('{id}')?.textContent === '{t}'");
    for (_, id, _) in CASES {
        cdp.wait_for(&text(id, &format!("{id} v1 0")), T);
    }
    cdp.wait_for(
        "performance.getEntriesByType('resource').some(e => e.name.includes('/@vite/client'))",
        T,
    );
    std::thread::sleep(Duration::from_millis(300));
    for (_, id, _) in CASES {
        cdp.eval(&format!("document.getElementById('{id}').click()"))
            .unwrap();
        cdp.wait_for(&text(id, &format!("{id} v1 1")), T);
    }
    let marker = cdp.eval("window.__marker").unwrap();
    let same_page = format!("window.__marker === {marker}");

    let kept = |id: &str| !matches!(id, "anonfn" | "anonarrow" | "reset" | "klass");
    for (file, id, template) in CASES {
        app.write(&format!("src/cases/{file}"), &source(template, "v2", 0));
        let want = format!("{id} v2 {}", if kept(id) { 1 } else { 0 });
        cdp.wait_for(&text(id, &want), T);
        assert_eq!(cdp.eval(&same_page).unwrap(), true, "{id}: page reloaded");
        // The other components kept their state.
        for (_, other, _) in CASES {
            if other != id {
                let n = cdp
                    .eval(&format!(
                        "document.getElementById('{other}').textContent.slice(-1)"
                    ))
                    .unwrap();
                let expect = if kept(other)
                    || CASES.iter().position(|c| c.1 == *other)
                        > CASES.iter().position(|c| c.1 == *id)
                {
                    "1"
                } else {
                    "0"
                };
                assert_eq!(n, expect, "after editing {id}: {other}");
            }
        }
    }
    // Changing a component's hooks remounts it, even where state is otherwise kept.
    let memo = CASES.iter().find(|c| c.1 == "memo").unwrap();
    app.write("src/cases/memo.jsx", &source(memo.2, "v3", 5));
    cdp.wait_for(&text("memo", "memo v3 5"), T);
    assert_eq!(cdp.eval(&same_page).unwrap(), true);
    // A later plain edit keeps the (new) state again.
    cdp.eval("document.getElementById('memo').click()").unwrap();
    cdp.wait_for(&text("memo", "memo v3 6"), T);
    app.write("src/cases/memo.jsx", &source(memo.2, "v4", 5));
    cdp.wait_for(&text("memo", "memo v4 6"), T);
    assert_eq!(cdp.eval(&same_page).unwrap(), true);
    for l in dev.log_lines() {
        eprintln!("{l}");
    }
}
