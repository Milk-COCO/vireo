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
//! 生命周期：用户 main 结束（正常返回或 panic）即关闭所有窗口并退出进程
//! （见 `vireo::App::new` 文档）。loop panic 经 `ThreadHandle` 以 `Err` 交付，
//! 不会留下冻住的窗口与僵尸进程。
//!
//! 属性（可组合）：
//! - `descriptor = <expr>`：自定义 wgpu `InstanceDescriptor`。
//! - `fonts = |&mut AndroidFontConfig| { .. }`：Android 系统字体白名单定制。
//!   **必须在 `App` 构造前求值** —— 而用户函数体是在 `App::new()` 内部才执行的，
//!   所以宏路径下这是唯一时机（手写 `App::new(...)` 时可改用
//!   `vireo::text::android_fonts::configure`）。
//!
//! 两种用户写法都支持：
//! - `async fn main() { ... app.window(...) ... }`：`app` 由宏注入（闭包参数），向后兼容。
//! - `async fn main(app: App) { ... app.window(...) ... }`：`app` 为显式参数，更直观；
//!   宏把该参数名用作注入绑定，函数体不变。二者完全等价。

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Expr, Ident, ItemFn, Result, Token};

/// `#[vireo::main]` 的可选属性（可组合、顺序任意）：
///
/// - `descriptor = <InstanceDescriptor 表达式>`
/// - `fonts = |&mut AndroidFontConfig| { .. }` —— 在 `App` 构造**之前**跑，
///   用于 Android 系统字体白名单定制（见 `vireo::text::android_fonts`）。
struct MainArgs {
    descriptor: Option<Expr>,
    fonts: Option<Expr>,
}

impl Parse for MainArgs {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut descriptor = None;
        let mut fonts = None;
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            match key.to_string().as_str() {
                "descriptor" => {
                    if descriptor.is_some() {
                        return Err(syn::Error::new(key.span(), "`descriptor` 重复指定"));
                    }
                    input.parse::<Token![=]>()?;
                    descriptor = Some(input.parse::<Expr>()?);
                }
                "fonts" => {
                    if fonts.is_some() {
                        return Err(syn::Error::new(key.span(), "`fonts` 重复指定"));
                    }
                    input.parse::<Token![=]>()?;
                    fonts = Some(input.parse::<Expr>()?);
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "未知属性 `{other}`，仅支持 `descriptor = <expr>` 与 `fonts = <expr>`"
                        ),
                    ));
                }
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        Ok(MainArgs { descriptor, fonts })
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

    let ItemFn {
        vis, sig, block, ..
    } = input;
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

    // `fonts = <expr>` 必须在 `App` 构造**之前**求值：`App::new()` 内部就会
    // 建 `GpuContext` → `TextContext` → Android 系统字体 install()，用户函数体
    // 是在那**之后**才被调用的（宏把 body 包成闭包交给 App::new）。所以宏路径下
    // 用户自己没有任何「App 构造前」的时机，只能靠这个属性。
    let fonts_setup = args.fonts.map(|f| {
        quote! { vireo::text::android_fonts::configure(#f); }
    });

    // 安卓没有 `fn main`：同一份用户源码在安卓 target 下展开为
    // GameActivity 胶水层按符号名查找的入口 `android_main`，
    // 由 `vireo::app::android_main_entry` 把 `AndroidApp` 存进进程级单例后转
    // `App::new` —— 与桌面走同一个构造器，公开 API 上不区分平台。
    if let Some(desc) = args.descriptor {
        let entry = quote! { vireo::App::with_descriptor(#desc, #body) };
        return Ok(quote! {
            #[cfg(not(target_os = "android"))]
            #vis fn main() #ret {
                #fonts_setup
                #entry;
            }
            #[cfg(target_os = "android")]
            #[unsafe(no_mangle)]
            pub extern "C" fn android_main(app: vireo::android::AndroidApp) {
                #fonts_setup
                vireo::app::android_main_entry_with_descriptor(app, #desc, #body);
            }
        });
    }

    let entry = quote! { vireo::App::new(#body) };
    Ok(quote! {
        #[cfg(not(target_os = "android"))]
        #vis fn main() #ret {
            #fonts_setup
            #entry;
        }
        #[cfg(target_os = "android")]
        #[unsafe(no_mangle)]
        pub extern "C" fn android_main(app: vireo::android::AndroidApp) {
            #fonts_setup
            vireo::app::android_main_entry(app, #body);
        }
    })
}
