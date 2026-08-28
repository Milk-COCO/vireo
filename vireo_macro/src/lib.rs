//! `#[vireo::main]` 过程宏。
//!
//! 把 `async fn main() { ... }` 改写为进程入口 `fn main()`，在 **OS 主线程**上构造 `App`、
//! 在独立的 `vireo-main` 线程上运行用户代码（由 [`vireo::App::new`] 用 `pollster::block_on`
//! 驱动），并在 OS 主线程跑 winit 事件循环（满足 winit 的线程要求，含 macOS）。
//!
//! 这样 `App::run` / `App::spawn` / `App::loops` 的全部公开 API 保持不变，且用户代码
//! **不**在 OS 主线程上运行；仅事件循环的所有权从被 spawn 的渲染线程交回 OS 主线程，
//! 从而合法地创建 winit `EventLoop`（无需 `any_thread` 逃逸口）。
//!
//! 宏生成的 `fn main` 等价于 `vireo::App::new(|app| async move { ... })`（或带
//! `descriptor` 属性时 `vireo::App::with_descriptor(EXPR, |app| async move { ... })`）。
//!
//! 两种用户写法都支持：
//! - `async fn main() { ... app.window(...) ... }`：`app` 由宏注入（闭包参数），向后兼容。
//! - `async fn main(app: App) { ... app.window(...) ... }`：`app` 为显式参数，更直观；
//!   宏把该参数名用作注入绑定，函数体不变。二者完全等价。

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Expr, Ident, ItemFn, Result, Token};

/// `#[vireo::main]` 的可选属性：`descriptor = <InstanceDescriptor 表达式>`。
struct MainArgs {
    descriptor: Option<Expr>,
}

impl Parse for MainArgs {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut descriptor = None;
        if !input.is_empty() {
            let key: Ident = input.parse()?;
            if key != "descriptor" {
                return Err(syn::Error::new(
                    key.span(),
                    "未知属性，仅支持 `descriptor = <expr>`",
                ));
            }
            input.parse::<Token![=]>()?;
            let expr: Expr = input.parse()?;
            descriptor = Some(expr);
        }
        Ok(MainArgs { descriptor })
    }
}

#[proc_macro_attribute]
pub fn main(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match syn::parse::<MainArgs>(attr) {
        Ok(a) => a,
        Err(e) => return e.into_compile_error().into(),
    };
    let input = syn::parse_macro_input!(item as ItemFn);
    expand(input, args)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand(input: ItemFn, args: MainArgs) -> Result<proc_macro2::TokenStream> {
    if input.sig.ident != "main" {
        return Err(syn::Error::new_spanned(
            &input.sig.ident,
            "#[vireo::main] 只能用于名为 `main` 的函数",
        ));
    }

    let ItemFn { vis, sig, block, .. } = input;
    let ret = &sig.output;

    // 参数形式：无参数 → 注入 `app`；单参数 → 用其绑定名注入。两种写法等价。
    let param_ident = match sig.inputs.len() {
        0 => syn::Ident::new("app", proc_macro2::Span::call_site()),
        1 => {
            let arg = sig.inputs.first().unwrap();
            match arg {
                syn::FnArg::Typed(pt) => {
                    if let syn::Pat::Ident(pi) = &*pt.pat {
                        pi.ident.clone()
                    } else {
                        return Err(syn::Error::new_spanned(
                            &pt.pat,
                            "#[vireo::main] 的参数必须是简单绑定（如 `app: App`）",
                        ));
                    }
                }
                syn::FnArg::Receiver(_) => {
                    return Err(syn::Error::new_spanned(
                        arg,
                        "#[vireo::main] 不支持 `self` 参数",
                    ));
                }
            }
        }
        _ => {
            return Err(syn::Error::new_spanned(
                &sig.inputs,
                "#[vireo::main] 的 main 最多只能带一个参数",
            ));
        }
    };

    // 用户原函数体包成 `|<param>| async move { ... }`，`<param>` 由 `App::new` / `with_descriptor`
    // 注入——用户代码通过它访问 `App`。无论原签名是否 async，都包成 async 块。
    let body = quote! { |#param_ident| async move #block };

    let entry = if let Some(desc) = args.descriptor {
        quote! { vireo::App::with_descriptor(#desc, #body) }
    } else {
        quote! { vireo::App::new(#body) }
    };

    Ok(quote! {
        #vis fn main() #ret {
            #entry;
        }
    })
}
