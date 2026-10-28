//! `unicorn/no-canvas-to-image`: `drawImage(await loadImage(canvas.toDataURL()))` and
//! `putImageData(ctx.getImageData(...))` round-trip a canvas through an image; draw the
//! canvas directly. The image argument may also be a `const` used only there.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_util_h2::direct_or_single_use_const;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-canvas-to-image",
        "Prefer drawing canvases directly instead of converting them to images.",
        NO_PRESET,
        Category::Pedantic,
    ),
    create: create_default::<NoCanvasToImage>,
};

#[derive(Debug, Default)]
struct NoCanvasToImage;

fn canvas_like(ast: &Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    let name = match *ast.node(id) {
        Node::Ident { .. } => ident_name(ast, id),
        Node::Member { prop, .. } => ident_name(ast, prop),
        _ => None,
    };
    name.is_some_and(|n| n.to_lowercase().contains("canvas"))
}

/// A plain `obj.method(...)` call (no optional chaining).
fn plain_call<'a>(
    ast: &'a Ast,
    id: NodeId,
    method: &str,
) -> Option<super::unicorn_ast::MethodCall<'a>> {
    method_call(ast, id).filter(|m| m.plain() && m.name == method)
}

fn is_load_image_of_data_url(ast: &Ast, id: NodeId) -> bool {
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(id)
    else {
        return false;
    };
    if ident_name(ast, skip_parens(ast, callee)) != Some("loadImage")
        || !matches!(ast.node(skip_parens(ast, callee)), Node::Ident { .. })
    {
        return false;
    }
    let Some(&first) = ast.list(args).first() else {
        return false;
    };
    let first = skip_parens(ast, first);
    plain_call(ast, first, "toDataURL").is_some_and(|m| canvas_like(ast, m.obj))
}

impl Rule for NoCanvasToImage {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node).filter(|m| m.plain()) else {
            return;
        };
        let draw = match call.name {
            "drawImage" => true,
            "putImageData" => false,
            _ => return,
        };
        let Some(&arg) = call.args.first() else {
            return;
        };
        let arg = skip_parens(ast, arg);
        if matches!(ast.node(arg), Node::Spread { .. }) {
            return;
        }
        let Some(source) = direct_or_single_use_const(cx, arg) else {
            return;
        };
        let hit = if draw {
            is_load_image_of_data_url(ast, source)
        } else {
            plain_call(ast, source, "getImageData").is_some()
        };
        if hit {
            cx.report(
                cx.span(arg),
                "Prefer drawing the canvas directly with `drawImage()`.",
            )
            .id("no-canvas-to-image");
        }
    }
}
