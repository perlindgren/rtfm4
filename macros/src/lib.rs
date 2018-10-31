// #![deny(warnings)]
#![recursion_limit = "128"]

extern crate proc_macro;
extern crate proc_macro2;
extern crate quote;
extern crate rand;
extern crate syn;

use proc_macro::TokenStream;
use syn::parse_macro_input;

mod analyze;
mod check;
mod codegen;
mod post_check;
mod syntax;

/// Attribute to declare a RTFM application
///
/// **WIP documentation**
///
/// This attribute must be applied to a `const` item of type `()`. The `const` item is effectively
/// used as a `mod` item: its value must be a block that contains items commonly found in modules,
/// like functions and `static` variables.
///
/// The items allowed in the block value of the `const` item are specified below:
///
/// # `static [mut]` variables
///
/// These variables are used as *resources*. Resources can be owned by tasks or shared between them. ``
///
/// # `fn`
///
/// Functions must contain *one* of the following attributes: `init`, `idle`, `interrupt`,
/// `exception` or `task`. The attribute defines the role of the function in the application.
///
/// ## `#[init]`
///
/// This attribute indicates that the function is to be used as the initialization function. There
/// must be exactly one instance of the `init` attribute inside the `app`.
///
/// ## `#[idle]`
///
/// ## `#[interrupt]`
///
/// This attribute must be applied to a function with signature `[unsafe] fn() [-> !]`. The
/// attribute accepts the following arguments
///
/// - `priority = <integer>`. This is the static priority of the interrupt handler.
///
/// - `resources = [<resource-a>, <resource-b>, ..]`. Same as `init.resources`
///
/// - `schedule = [<task-a>, <task-b>, ..]`. Same as `init.schedule`
///
/// - `spawn = [<task-a>, <task-b>, ..]`. Same as `init.spawn`
///
/// ### `priority`
///
/// ## `#[exception]`
///
/// ## `#[task]`
///
/// # `extern` block
#[proc_macro_attribute]
pub fn app(args: TokenStream, input: TokenStream) -> TokenStream {
    // Parse
    let args = parse_macro_input!(args as syntax::AppArgs);
    let items = parse_macro_input!(input as syntax::Input).items;

    let app = match syntax::App::parse(items, args) {
        Err(e) => return e.to_compile_error().into(),
        Ok(app) => app,
    };

    // Check the specification
    if let Err(e) = check::app(&app) {
        return e.to_compile_error().into();
    }

    // Ceiling analysis
    let analysis = analyze::app(&app);

    // Post-analysis check
    if let Err(e) = post_check::ownerships(&app, &analysis.ownerships) {
        return e.to_compile_error().into();
    }

    // Code generation
    codegen::app(&app, &analysis)
}
