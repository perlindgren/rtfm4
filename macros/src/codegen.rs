use proc_macro::TokenStream;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use proc_macro2::Span;
use quote::quote;
use rand::{Rng, SeedableRng};
use syn::{ArgCaptured, Ident, IntSuffix, LitInt};

use analyze::{Analysis, Ownership};
use syntax::{App, Idents, Static};

// NOTE to avoid polluting the user namespaces we map some identifiers to pseudo-hygienic names.
// In some instances we also use the pseudo-hygienic names for safety, for example the user should
// not modify the priority field of resources.
type Aliases = HashMap<Ident, Ident>;

pub struct Context {
    // Alias
    baseline: Ident,
    // Dispatcher -> Alias (`enum`)
    enums: HashMap<u8, Ident>,
    // Task -> Alias (`static` / resource)
    free_queues: Aliases,
    // Alias (`fn`)
    idle: Ident,
    // Alias (`fn`)
    init: Ident,
    // Task -> Alias (`static`)
    inputs: Aliases,
    // Alias
    priority: Ident,
    // Dispatcher -> Alias (`static` / resource)
    ready_queues: HashMap<u8, Ident>,
    // For non-singletons this maps the resource name to its `static mut` variable name
    // For singletons this maps the resource name to the proxy struct name
    resources: Aliases,
    // Task -> Alias (`static`)
    scheduleds: Aliases,
    // Task -> Alias (`fn`)
    spawn_fn: Aliases,
    // Alias (`enum`)
    schedule_enum: Ident,
    // Task -> Alias (`fn`)
    schedule_fn: Aliases,
    tasks: Aliases,
    // Alias (`struct` / `static mut`)
    timer_queue: Ident,
}

impl Default for Context {
    fn default() -> Self {
        Context {
            baseline: mk_ident(),
            enums: HashMap::new(),
            free_queues: Aliases::new(),
            idle: mk_ident(),
            init: mk_ident(),
            inputs: Aliases::new(),
            priority: mk_ident(),
            ready_queues: HashMap::new(),
            resources: Aliases::new(),
            scheduleds: Aliases::new(),
            spawn_fn: Aliases::new(),
            schedule_enum: mk_ident(),
            schedule_fn: Aliases::new(),
            tasks: Aliases::new(),
            timer_queue: mk_ident(),
        }
    }
}

pub fn app(app: &App, analysis: &Analysis) -> TokenStream {
    let mut ctxt = Context::default();

    let device = &app.args.device;

    let resources = resources(&mut ctxt, &app, analysis);

    let tasks = tasks(&mut ctxt, &app, analysis);

    let (dispatchers_data, dispatchers) = dispatchers(&mut ctxt, &app, analysis);

    let init_fn = init(&mut ctxt, &app, analysis);

    let post_init = post_init(&ctxt, &app, analysis);

    let (idle_fn, idle_expr) = idle(&mut ctxt, &app, analysis);

    let exceptions = exceptions(&mut ctxt, app, analysis);

    let (root_interrupts, scoped_interrupts) = interrupts(&mut ctxt, app, analysis);

    let spawn = spawn(&mut ctxt, app, analysis);

    let schedule = schedule(&ctxt, app);

    let timer_queue = timer_queue(&ctxt, app, analysis);

    let pre_init = pre_init(&ctxt, analysis);

    let assertions = assertions(app, analysis);

    let init = &ctxt.init;
    quote!(
        #resources

        #spawn

        #timer_queue

        #schedule

        #dispatchers_data

        #(#exceptions)*

        #root_interrupts

        // We put these items into a pseudo-module to avoid a collision between the `interrupt`
        // import and user code
        const APP: () = {
            use #device::interrupt;

            #scoped_interrupts

            #(#dispatchers)*
        };

        #(#tasks)*

        #init_fn

        #idle_fn

        #[allow(unsafe_code)]
        #[rtfm::export::entry]
        #[doc(hidden)]
        fn main() -> ! {
            #assertions

            rtfm::export::interrupt::disable();

            #pre_init

            #init(rtfm::Peripherals {
                CBP: p.CBP,
                CPUID: p.CPUID,
                DCB: &mut p.DCB,
                FPB: p.FPB,
                FPU: p.FPU,
                ITM: p.ITM,
                MPU: p.MPU,
                SCB: &mut p.SCB,
                TPIU: p.TPIU,
            });

            #post_init

            unsafe { rtfm::export::interrupt::enable() }

            #idle_expr
        }
    )
    .into()
}

fn resources(ctxt: &mut Context, app: &App, analysis: &Analysis) -> proc_macro2::TokenStream {
    let mut items = vec![];
    for (name, res) in &app.resources {
        let attrs = &res.attrs;
        let mut_ = &res.mutability;
        let ty = &res.ty;
        let expr = &res.expr;

        if res.singleton {
            items.push(quote!(
                #(#attrs)*
                static #mut_ #name: #ty = #expr;
            ));

            let alias = mk_ident();
            if let Some(Ownership::Shared { ceiling }) = analysis.ownerships.get(name) {
                items.push(mk_resource(
                    ctxt,
                    &alias,
                    quote!(#name),
                    *ceiling,
                    quote!(&mut <#name as owned_singleton::Singleton>::new()),
                    app,
                ))
            }

            ctxt.resources.insert(name.clone(), alias);
        } else {
            let alias = mk_ident();
            let symbol = format!("{}::{}", name, alias);

            items.push(
                expr.as_ref()
                    .map(|expr| {
                        quote!(
                            #(#attrs)*
                            #[export_name = #symbol]
                            static mut #alias: #ty = #expr;
                        )
                    })
                    .unwrap_or_else(|| {
                        quote!(
                            #(#attrs)*
                            #[export_name = #symbol]
                            static mut #alias: rtfm::export::MaybeUninit<#ty> =
                                rtfm::export::MaybeUninit::uninitialized();
                        )
                    }),
            );

            if let Some(Ownership::Shared { ceiling }) = analysis.ownerships.get(name) {
                if res.mutability.is_some() {
                    let ptr = if res.expr.is_none() {
                        quote!(unsafe { #alias.get_mut() })
                    } else {
                        quote!(unsafe { &mut #alias })
                    };

                    items.push(mk_resource(ctxt, name, quote!(#ty), *ceiling, ptr, app))
                }
            }

            ctxt.resources.insert(name.clone(), alias);
        }
    }

    quote!(#(#items)*)
}

fn init(ctxt: &mut Context, app: &App, analysis: &Analysis) -> proc_macro2::TokenStream {
    let attrs = &app.init.attrs;
    let locals = mk_locals(&app.init.statics, true);
    let stmts = &app.init.stmts;
    let assigns = app
        .init
        .assigns
        .iter()
        .map(|assign| {
            if app
                .resources
                .get(&assign.left)
                .map(|r| r.expr.is_none())
                .unwrap_or(false)
            {
                let alias = &ctxt.resources[&assign.left];
                let expr = &assign.right;
                quote!(unsafe { #alias.set(#expr); })
            } else {
                let left = &assign.left;
                let right = &assign.right;
                quote!(#left = #right;)
            }
        })
        .collect::<Vec<_>>();

    let prelude = prelude(
        ctxt,
        Kind::Init,
        &app.init.args.resources,
        &app.init.args.spawn,
        &app.init.args.schedule,
        app,
        255,
        analysis,
    );

    let module = module(
        ctxt,
        Kind::Init,
        !app.init.args.schedule.is_empty(),
        !app.init.args.spawn.is_empty(),
    );

    let device = &app.args.device;
    let baseline = &ctxt.baseline;
    let init = &ctxt.init;
    let name = format!("init::{}", init);
    quote!(
        #module

        #(#attrs)*
        #[export_name = #name]
        fn #init(core: rtfm::Peripherals) {
            #(#locals)*

            let #baseline = rtfm::Instant::artificial(0);

            #prelude

            let device = unsafe { #device::Peripherals::steal() };

            let start = #baseline;

            #(#stmts)*

            #(#assigns)*
        }
    )
}

fn post_init(ctxt: &Context, app: &App, analysis: &Analysis) -> proc_macro2::TokenStream {
    let mut exprs = vec![];

    // TODO turn the assertions that check that the priority is not larger than what's supported by
    // the device into compile errors
    let device = &app.args.device;
    let nvic_prio_bits = quote!(#device::NVIC_PRIO_BITS);
    for (name, interrupt) in &app.interrupts {
        let priority = interrupt.args.priority;
        exprs.push(quote!(p.NVIC.enable(#device::Interrupt::#name)));
        exprs.push(quote!(assert!(#priority <= (1 << #nvic_prio_bits))));
        exprs.push(quote!(p.NVIC.set_priority(
            #device::Interrupt::#name,
            ((1 << #nvic_prio_bits) - #priority) << (8 - #nvic_prio_bits),
        )));
    }

    for (name, exception) in &app.exceptions {
        let priority = exception.args.priority;
        exprs.push(quote!(assert!(#priority <= (1 << #nvic_prio_bits))));
        exprs.push(quote!(p.SCB.set_priority(
            rtfm::export::SystemHandler::#name,
            ((1 << #nvic_prio_bits) - #priority) << (8 - #nvic_prio_bits),
        )));
    }

    if !analysis.timer_queue.tasks.is_empty() {
        let priority = analysis.timer_queue.priority;
        exprs.push(quote!(assert!(#priority <= (1 << #nvic_prio_bits))));
        exprs.push(quote!(p.SCB.set_priority(
            rtfm::export::SystemHandler::SysTick,
            ((1 << #nvic_prio_bits) - #priority) << (8 - #nvic_prio_bits),
        )));
    }

    for (priority, dispatcher) in &analysis.dispatchers {
        let name = &dispatcher.interrupt;
        exprs.push(quote!(p.NVIC.enable(#device::Interrupt::#name)));
        exprs.push(quote!(assert!(#priority <= (1 << #nvic_prio_bits))));
        exprs.push(quote!(p.NVIC.set_priority(
            #device::Interrupt::#name,
            ((1 << #nvic_prio_bits) - #priority) << (8 - #nvic_prio_bits),
        )));
    }

    if app.idle.is_none() {
        // Set SLEEPONEXIT bit to enter sleep mode when returning from ISR
        exprs.push(quote!(p.SCB.scr.modify(|r| r | 1 << 1)));
    }

    // Enable and start the system timer
    if !analysis.timer_queue.tasks.is_empty() {
        let tq = &ctxt.timer_queue;
        exprs.push(quote!(#tq.get_mut().syst.set_clock_source(rtfm::export::SystClkSource::Core)));
        exprs.push(quote!(#tq.get_mut().syst.enable_counter()));
    }

    // Enable cycle counter
    exprs.push(quote!(p.DCB.enable_trace()));
    exprs.push(quote!(p.DWT.enable_cycle_counter()));

    quote!(unsafe {
        #(#exprs;)*
    })
}

/// This function creates creates a module for `init` / `idle` / a `task` (see `kind` argument)
fn module(ctxt: &mut Context, kind: Kind, schedule: bool, spawn: bool) -> proc_macro2::TokenStream {
    let mut items = vec![];

    let name = kind.ident();
    let priority = &ctxt.priority;
    let baseline = &ctxt.baseline;

    if schedule {
        items.push(quote!(
            /// Tasks that can be scheduled from this context
            #[derive(Clone, Copy)]
            pub struct Schedule<'a> {
                #[doc(hidden)]
                pub #priority: &'a core::cell::Cell<u8>,
            }
        ));
    }

    if spawn {
        if kind.is_idle() {
            items.push(quote!(
                /// Tasks that can be spawned from this context
                #[derive(Clone, Copy)]
                pub struct Spawn<'a> {
                    #[doc(hidden)]
                    pub #priority: &'a core::cell::Cell<u8>,
                }
            ));
        } else {
            items.push(quote!(
                /// Tasks that can be spawned from this context
                #[derive(Clone, Copy)]
                pub struct Spawn<'a> {
                    #[doc(hidden)]
                    pub #baseline: rtfm::Instant,
                    #[doc(hidden)]
                    pub #priority: &'a core::cell::Cell<u8>,
                }
            ));
        }
    }

    if !items.is_empty() {
        let doc = match kind {
            Kind::Exception(_) => "Exception handler",
            Kind::Idle => "Idle loop",
            Kind::Init => "Initialization function",
            Kind::Interrupt(_) => "Interrupt handler",
            Kind::Task(_) => "Software task",
        };

        quote!(
            #[doc = #doc]
            pub mod #name {
                #(#items)*
            }
        )
    } else {
        quote!()
    }
}

/// The prelude injects `resources`, `spawn`, `schedule` and `start` / `scheduled` (all values) into
/// a function scope
fn prelude(
    ctxt: &mut Context,
    kind: Kind,
    resources: &Idents,
    spawn: &Idents,
    schedule: &Idents,
    app: &App,
    logical_prio: u8,
    analysis: &Analysis,
) -> proc_macro2::TokenStream {
    let mut items = vec![];

    let lt = if kind.runs_once() {
        quote!('static)
    } else {
        quote!('a)
    };

    let module = kind.ident();

    let priority = &ctxt.priority;
    if !resources.is_empty() {
        let mut defs = vec![];
        let mut exprs = vec![];

        // NOTE This field is just to avoid unused type parameter errors around `'a`
        defs.push(quote!(#[allow(dead_code)] #priority: &'a core::cell::Cell<u8>));
        exprs.push(quote!(#priority));

        let mut may_call_lock = false;
        let mut needs_unsafe = false;
        for name in resources {
            let res = &app.resources[name];
            let initialized = res.expr.is_some();
            let singleton = res.singleton;
            let mut_ = res.mutability;
            let ty = &res.ty;

            if kind.is_init() {
                let mut force_mut = false;
                if !analysis.ownerships.contains_key(name) {
                    // owned by Init
                    if singleton {
                        needs_unsafe = true;
                        defs.push(quote!(#name: #name));
                        exprs.push(quote!(#name: <#name as owned_singleton::Singleton>::new()));
                        continue;
                    } else {
                        defs.push(quote!(#name: &'static #mut_ #ty));
                    }
                } else {
                    // owned by someone else
                    if singleton {
                        needs_unsafe = true;
                        defs.push(quote!(#name: &'a mut #name));
                        exprs
                            .push(quote!(#name: &mut <#name as owned_singleton::Singleton>::new()));
                        continue;
                    } else {
                        force_mut = true;
                        defs.push(quote!(#name: &'a mut #ty));
                    }
                }

                let alias = &ctxt.resources[name];
                // Resources assigned to init are always const initialized
                if force_mut {
                    needs_unsafe = true;
                    exprs.push(quote!(#name: &mut #alias));
                } else {
                    exprs.push(quote!(#name: &#mut_ #alias));
                }
            } else {
                let ownership = &analysis.ownerships[name];

                if ownership.needs_lock(logical_prio) {
                    may_call_lock = true;
                    if singleton {
                        let alias = &ctxt.resources[name];
                        if mut_.is_none() {
                            needs_unsafe = true;
                            defs.push(quote!(#name: &'a #name));
                            exprs
                                .push(quote!(#name: &<#name as owned_singleton::Singleton>::new()));
                            continue;
                        } else {
                            // Generate a resource proxy
                            defs.push(quote!(#name: #alias<'a>));
                            exprs.push(quote!(#name: #alias { #priority }));
                            continue;
                        }
                    } else {
                        if mut_.is_none() {
                            defs.push(quote!(#name: &'a #ty));
                        } else {
                            // Generate a resource proxy
                            defs.push(quote!(#name: #name<'a>));
                            exprs.push(quote!(#name: #name { #priority }));
                            continue;
                        }
                    }
                } else {
                    if singleton {
                        if kind.runs_once() {
                            needs_unsafe = true;
                            defs.push(quote!(#name: #name));
                            exprs.push(quote!(#name: <#name as owned_singleton::Singleton>::new()));
                        } else {
                            needs_unsafe = true;
                            defs.push(quote!(#name: &'a mut #name));
                            exprs.push(
                                quote!(#name: &mut <#name as owned_singleton::Singleton>::new()),
                            );
                        }
                        continue;
                    } else {
                        defs.push(quote!(#name: &#lt #mut_ #ty));
                    }
                }

                let alias = &ctxt.resources[name];
                needs_unsafe = true;
                if initialized {
                    exprs.push(quote!(#name: &#mut_ #alias));
                } else {
                    let method = if mut_.is_some() {
                        quote!(get_mut)
                    } else {
                        quote!(get_ref)
                    };
                    exprs.push(quote!(#name: #alias.#method() ));
                }
            }
        }

        let unsafety = if needs_unsafe {
            Some(quote!(unsafe))
        } else {
            None
        };
        items.push(quote!(
            #[allow(unsafe_code)]
            #[allow(unused_mut)]
            let mut resources = {
                #[allow(non_snake_case)]
                struct Resources<'a> { #(#defs,)* }

                #unsafety { Resources { #(#exprs,)* } }
            };
        ));

        if may_call_lock {
            items.push(quote!(
                use rtfm::Mutex;
            ));
        }
    }

    if !spawn.is_empty() {
        // Populate `spawn_fn`
        for task in spawn {
            if ctxt.spawn_fn.contains_key(task) {
                continue;
            }

            ctxt.spawn_fn.insert(task.clone(), mk_ident());
        }

        if kind.is_idle() {
            items.push(quote!(
                let spawn = #module::Spawn { #priority };
            ));
        } else {
            let baseline = &ctxt.baseline;
            items.push(quote!(
                let spawn = #module::Spawn { #priority, #baseline };
            ));
        }
    }

    if !schedule.is_empty() {
        // Populate `schedule_fn`
        for task in schedule {
            if ctxt.schedule_fn.contains_key(task) {
                continue;
            }

            ctxt.schedule_fn.insert(task.clone(), mk_ident());
        }

        items.push(quote!(
            use rtfm::U32Ext;

            let schedule = #module::Schedule { #priority };
        ));
    }

    if items.is_empty() {
        quote!()
    } else {
        quote!(
            let ref #priority = core::cell::Cell::new(#logical_prio);

            #(#items)*
        )
    }
}

fn idle(
    ctxt: &mut Context,
    app: &App,
    analysis: &Analysis,
) -> (proc_macro2::TokenStream, proc_macro2::TokenStream) {
    if let Some(idle) = app.idle.as_ref() {
        let attrs = &idle.attrs;
        let locals = mk_locals(&idle.statics, true);
        let stmts = &idle.stmts;

        let prelude = prelude(
            ctxt,
            Kind::Idle,
            &idle.args.resources,
            &idle.args.spawn,
            &idle.args.schedule,
            app,
            0,
            analysis,
        );

        let module = module(
            ctxt,
            Kind::Idle,
            !idle.args.schedule.is_empty(),
            !idle.args.spawn.is_empty(),
        );

        let idle = &ctxt.idle;

        let name = format!("idle::{}", idle);
        (
            quote!(
                #module

                #(#attrs)*
                #[export_name = #name]
                fn #idle() -> ! {
                    #(#locals)*

                    #prelude

                    #(#stmts)*
                }),
            quote!(#idle()),
        )
    } else {
        (
            quote!(),
            quote!(loop {
                rtfm::export::wfi();
            }),
        )
    }
}

fn exceptions(ctxt: &mut Context, app: &App, analysis: &Analysis) -> Vec<proc_macro2::TokenStream> {
    app.exceptions
        .iter()
        .map(|(ident, exception)| {
            let attrs = &exception.attrs;
            let statics = &exception.statics;
            let stmts = &exception.stmts;

            let prelude = prelude(
                ctxt,
                Kind::Exception(ident.clone()),
                &exception.args.resources,
                &exception.args.spawn,
                &exception.args.schedule,
                app,
                exception.args.priority,
                analysis,
            );

            let module = module(
                ctxt,
                Kind::Exception(ident.clone()),
                !exception.args.schedule.is_empty(),
                !exception.args.spawn.is_empty(),
            );

            let baseline = &ctxt.baseline;
            quote!(
                #module

                #[rtfm::export::exception]
                #[doc(hidden)]
                #(#attrs)*
                fn #ident() {
                    #(#statics)*

                    let #baseline = rtfm::Instant::now();

                    #prelude

                    #[allow(unused_variables)]
                    let start = #baseline;

                    rtfm::export::run(move || {
                        #(#stmts)*
                    })
                })
        })
        .collect()
}

fn interrupts(
    ctxt: &mut Context,
    app: &App,
    analysis: &Analysis,
) -> (proc_macro2::TokenStream, proc_macro2::TokenStream) {
    let mut root = vec![];
    let mut scoped = vec![];

    for (ident, interrupt) in &app.interrupts {
        let attrs = &interrupt.attrs;
        let statics = &interrupt.statics;
        let stmts = &interrupt.stmts;

        let prelude = prelude(
            ctxt,
            Kind::Interrupt(ident.clone()),
            &interrupt.args.resources,
            &interrupt.args.spawn,
            &interrupt.args.schedule,
            app,
            interrupt.args.priority,
            analysis,
        );

        root.push(module(
            ctxt,
            Kind::Interrupt(ident.clone()),
            !interrupt.args.schedule.is_empty(),
            !interrupt.args.spawn.is_empty(),
        ));

        let baseline = &ctxt.baseline;
        scoped.push(quote!(
            #[interrupt]
            #(#attrs)*
            fn #ident() {
                #(#statics)*

                let #baseline = rtfm::Instant::now();

                #prelude

                #[allow(unused_variables)]
                let start = #baseline;

                rtfm::export::run(move || {
                    #(#stmts)*
                })
            }));
    }

    (quote!(#(#root)*), quote!(#(#scoped)*))
}

fn tasks(ctxt: &mut Context, app: &App, analysis: &Analysis) -> proc_macro2::TokenStream {
    let mut items = vec![];
    for (name, task) in &app.tasks {
        let scheduleds_alias = mk_ident();
        let free_alias = mk_ident();
        let inputs_alias = mk_ident();
        let task_alias = mk_ident();

        let attrs = &task.attrs;
        let inputs = &task.inputs;
        let locals = mk_locals(&task.statics, false);
        let stmts = &task.stmts;

        let prelude = prelude(
            ctxt,
            Kind::Task(name.clone()),
            &task.args.resources,
            &task.args.spawn,
            &task.args.schedule,
            app,
            task.args.priority,
            analysis,
        );

        let ty = tuple_ty(inputs);

        let capacity_lit = mk_capacity_literal(analysis.capacities[name]);
        let capacity_ty = mk_typenum_capacity(analysis.capacities[name], true);

        let resource = mk_resource(
            ctxt,
            &free_alias,
            quote!(rtfm::export::FreeQueue<#capacity_ty>),
            *analysis.free_queues.get(name).unwrap_or(&0),
            quote!(#free_alias.get_mut()),
            app,
        );

        let baseline = ctxt.baseline.clone();
        let task_symbol = format!("{}::{}", name, task_alias);
        let scheduleds_symbol = format!("{}::SCHEDULED_TIMES::{}", name, scheduleds_alias);
        let inputs_symbol = format!("{}::INPUTS::{}", name, inputs_alias);
        let free_symbol = format!("{}::FREE_QUEUE::{}", name, free_alias);
        items.push(quote!(
            // FIXME(MaybeUninit) MaybeUninit won't be necessary when core::mem::MaybeUninit
            // stabilizes because heapless constructors will work in const context
            #[export_name = #free_symbol]
            static mut #free_alias: rtfm::export::MaybeUninit<
                    rtfm::export::FreeQueue<#capacity_ty>
                > = rtfm::export::MaybeUninit::uninitialized();

            #resource

            #[export_name = #inputs_symbol]
            static mut #inputs_alias: rtfm::export::MaybeUninit<[#ty; #capacity_lit]> =
                rtfm::export::MaybeUninit::uninitialized();

            #[export_name = #scheduleds_symbol]
            static mut #scheduleds_alias:
                rtfm::export::MaybeUninit<[rtfm::Instant; #capacity_lit]> =
                rtfm::export::MaybeUninit::uninitialized();

            #(#attrs)*
            #[export_name = #task_symbol]
            fn #task_alias(#baseline: rtfm::Instant, #(#inputs,)*) {
                #(#locals)*

                #prelude

                let scheduled = #baseline;

                #(#stmts)*
            }
        ));

        items.push(module(
            ctxt,
            Kind::Task(name.clone()),
            !task.args.schedule.is_empty(),
            !task.args.spawn.is_empty(),
        ));

        ctxt.scheduleds.insert(name.clone(), scheduleds_alias);
        ctxt.free_queues.insert(name.clone(), free_alias);
        ctxt.inputs.insert(name.clone(), inputs_alias);
        ctxt.tasks.insert(name.clone(), task_alias);
    }

    quote!(#(#items)*)
}

fn dispatchers(
    ctxt: &mut Context,
    app: &App,
    analysis: &Analysis,
) -> (proc_macro2::TokenStream, proc_macro2::TokenStream) {
    let mut data = vec![];
    let mut dispatchers = vec![];

    for (level, dispatcher) in &analysis.dispatchers {
        let ready_alias = mk_ident();
        let enum_alias = mk_ident();
        let tasks = &dispatcher.tasks;
        let capacity = mk_typenum_capacity(dispatcher.capacity, true);

        let symbol = format!("P{}::READY_QUEUE::{}", level, ready_alias);
        let e = quote!(rtfm::export);
        let ty = quote!(#e::ReadyQueue<#enum_alias, #capacity>);
        let ceiling = *analysis.ready_queues.get(&level).unwrap_or(&0);
        let resource = mk_resource(
            ctxt,
            &ready_alias,
            ty.clone(),
            ceiling,
            quote!(#ready_alias.get_mut()),
            app,
        );
        data.push(quote!(
            #[allow(dead_code)]
            #[allow(non_camel_case_types)]
            enum #enum_alias { #(#tasks,)* }

            #[export_name = #symbol]
            static mut #ready_alias: #e::MaybeUninit<#ty> = #e::MaybeUninit::uninitialized();

            #resource
        ));

        let interrupt = &dispatcher.interrupt;

        let arms = dispatcher
            .tasks
            .iter()
            .map(|task| {
                let inputs = &ctxt.inputs[task];
                let scheduleds = &ctxt.scheduleds[task];
                let free = &ctxt.free_queues[task];
                let pats = tuple_pat(&app.tasks[task].inputs);
                let alias = &ctxt.tasks[task];

                quote!(#enum_alias::#task => {
                    let baseline = ptr::read(#scheduleds.get_ref().get_unchecked(usize::from(index)));
                    let input = ptr::read(#inputs.get_ref().get_unchecked(usize::from(index)));
                    #free.get_mut().split().0.enqueue_unchecked(index);
                    let (#pats) = input;
                    #alias(baseline, #pats);
                })
            })
            .collect::<Vec<_>>();

        let attrs = &dispatcher.attrs;
        dispatchers.push(quote!(
            #(#attrs)*
            #[interrupt]
            unsafe fn #interrupt() {
                use core::ptr;

                rtfm::export::run(|| {
                    while let Some((task, index)) = #ready_alias.get_mut().split().1.dequeue() {
                        match task {
                            #(#arms)*
                        }
                    }
                });
            }
        ));

        ctxt.ready_queues.insert(*level, ready_alias);
        ctxt.enums.insert(*level, enum_alias);
    }

    (quote!(#(#data)*), quote!(#(#dispatchers)*))
}

fn spawn(ctxt: &Context, app: &App, analysis: &Analysis) -> proc_macro2::TokenStream {
    let mut items = vec![];

    // Generate `spawn` functions
    let device = &app.args.device;
    let priority = &ctxt.priority;
    let baseline = &ctxt.baseline;
    for (task, alias) in &ctxt.spawn_fn {
        let free = &ctxt.free_queues[task];
        let level = app.tasks[task].args.priority;
        let ready = &ctxt.ready_queues[&level];
        let enum_ = &ctxt.enums[&level];
        let dispatcher = &analysis.dispatchers[&level].interrupt;
        let inputs = &ctxt.inputs[task];
        let scheduleds = &ctxt.scheduleds[task];
        let args = &app.tasks[task].inputs;
        let ty = tuple_ty(args);
        let pats = tuple_pat(args);

        items.push(quote!(
            #[inline(always)]
            unsafe fn #alias(
                #baseline: rtfm::Instant,
                #priority: &core::cell::Cell<u8>,
                #(#args,)*
            ) -> Result<(), #ty> {
                use core::ptr;

                use rtfm::Mutex;

                if let Some(index) = (#free { #priority }).lock(|f| f.split().1.dequeue()) {
                    ptr::write(#inputs.get_mut().get_unchecked_mut(usize::from(index)), (#pats));
                    ptr::write(
                        #scheduleds.get_mut().get_unchecked_mut(usize::from(index)),
                        #baseline,
                    );

                    #ready { #priority }.lock(|rq| {
                        rq.split().0.enqueue_unchecked((#enum_::#task, index))
                    });

                    rtfm::pend(#device::Interrupt::#dispatcher);

                    Ok(())
                } else {
                    Err((#pats))
                }
            }
        ))
    }

    // Generate `spawn` structs; these call the `spawn` functions generated above
    for (name, spawn) in app.spawn_callers() {
        if spawn.is_empty() {
            continue;
        }

        let mut is_idle = name.to_string() == "idle";

        let mut methods = vec![];
        for task in spawn {
            let alias = &ctxt.spawn_fn[task];
            let inputs = &app.tasks[task].inputs;
            let ty = tuple_ty(inputs);
            let pats = tuple_pat(inputs);

            let instant = if is_idle {
                quote!(rtfm::Instant::now())
            } else {
                quote!(self.#baseline)
            };
            methods.push(quote!(
                #[allow(unsafe_code)]
                #[inline]
                pub fn #task(&self, #(#inputs,)*) -> Result<(), #ty> {
                    unsafe { #alias(#instant, &self.#priority, #pats) }
                }
            ));
        }

        items.push(quote!(
            impl<'a> #name::Spawn<'a> {
                #(#methods)*
            }
        ));
    }

    quote!(#(#items)*)
}

fn schedule(ctxt: &Context, app: &App) -> proc_macro2::TokenStream {
    let mut items = vec![];

    // Generate `schedule` functions
    let priority = &ctxt.priority;
    let timer_queue = &ctxt.timer_queue;
    for (task, alias) in &ctxt.schedule_fn {
        let free = &ctxt.free_queues[task];
        let enum_ = &ctxt.schedule_enum;
        let inputs = &ctxt.inputs[task];
        let scheduleds = &ctxt.scheduleds[task];
        let args = &app.tasks[task].inputs;
        let ty = tuple_ty(args);
        let pats = tuple_pat(args);

        items.push(quote!(
            #[inline(always)]
            unsafe fn #alias(
                #priority: &core::cell::Cell<u8>,
                instant: rtfm::Instant,
                #(#args,)*
            ) -> Result<(), #ty> {
                use core::ptr;

                use rtfm::Mutex;

                if let Some(index) = (#free { #priority }).lock(|f| f.split().1.dequeue()) {
                    ptr::write(#inputs.get_mut().get_unchecked_mut(usize::from(index)), (#pats));
                    ptr::write(
                        #scheduleds.get_mut().get_unchecked_mut(usize::from(index)),
                        instant,
                    );

                    let nr = rtfm::export::NotReady {
                        instant,
                        index,
                        task: #enum_::#task,
                    };

                    ({#timer_queue { #priority }}).lock(|tq| tq.enqueue_unchecked(nr));

                    Ok(())
                } else {
                    Err((#pats))
                }
            }
        ))
    }

    // Generate `Schedule` structs; these call the `schedule` functions generated above
    for (name, schedule) in app.schedule_callers() {
        if schedule.is_empty() {
            continue;
        }

        debug_assert!(!schedule.is_empty());

        let mut methods = vec![];
        for task in schedule {
            let alias = &ctxt.schedule_fn[task];
            let inputs = &app.tasks[task].inputs;
            let ty = tuple_ty(inputs);
            let pats = tuple_pat(inputs);

            methods.push(quote!(
                #[inline]
                pub fn #task(
                    &self,
                    instant: rtfm::Instant,
                    #(#inputs,)*
                ) -> Result<(), #ty> {
                    unsafe { #alias(&self.#priority, instant, #pats) }
                }
            ));
        }

        items.push(quote!(
            impl<'a> #name::Schedule<'a> {
                #(#methods)*
            }
        ));
    }

    quote!(#(#items)*)
}

fn timer_queue(ctxt: &Context, app: &App, analysis: &Analysis) -> proc_macro2::TokenStream {
    let tasks = &analysis.timer_queue.tasks;

    if tasks.is_empty() {
        return quote!();
    }

    let mut items = vec![];

    let enum_ = &ctxt.schedule_enum;
    items.push(quote!(
        #[allow(dead_code)]
        #[allow(non_camel_case_types)]
        #[derive(Clone, Copy)]
        enum #enum_ { #(#tasks,)* }
    ));

    let cap = mk_typenum_capacity(analysis.timer_queue.capacity, false);
    let tq = &ctxt.timer_queue;
    let symbol = format!("TIMER_QUEUE::{}", tq);
    items.push(quote!(
        #[export_name = #symbol]
        static mut #tq:
            rtfm::export::MaybeUninit<rtfm::export::TimerQueue<#enum_, #cap>> =
                rtfm::export::MaybeUninit::uninitialized();
    ));

    items.push(mk_resource(
        ctxt,
        tq,
        quote!(rtfm::export::TimerQueue<#enum_, #cap>),
        analysis.timer_queue.ceiling,
        quote!(#tq.get_mut()),
        app,
    ));

    let priority = &ctxt.priority;
    let device = &app.args.device;
    let arms = tasks
        .iter()
        .map(|task| {
            let level = app.tasks[task].args.priority;
            let tenum = &ctxt.enums[&level];
            let ready = &ctxt.ready_queues[&level];
            let dispatcher = &analysis.dispatchers[&level].interrupt;

            quote!(
                #enum_::#task => {
                    (#ready { #priority }).lock(|rq| {
                        rq.split().0.enqueue_unchecked((#tenum::#task, index))
                    });

                    rtfm::pend(#device::Interrupt::#dispatcher);
                }
            )
        })
        .collect::<Vec<_>>();

    let logical_prio = analysis.timer_queue.priority;
    items.push(quote!(
        #[rtfm::export::exception]
        #[doc(hidden)]
        unsafe fn SysTick() {
            use rtfm::Mutex;

            let ref #priority = core::cell::Cell::new(#logical_prio);

            rtfm::export::run(|| {
                rtfm::export::sys_tick(#tq { #priority }, |task, index| {
                    match task {
                        #(#arms)*
                    }
                });
            })
        }
    ));

    quote!(#(#items)*)
}

fn pre_init(ctxt: &Context, analysis: &Analysis) -> proc_macro2::TokenStream {
    let mut exprs = vec![];

    // FIXME(MaybeUninit) Because we are using a fake MaybeUninit we need to set the Option tag to
    // Some; otherwise the get_ref and get_mut could result in UB. Also heapless collections can't
    // be constructed in const context; we have to initialize them at runtime (i.e. here).

    // these are `MaybeUninit` arrays
    for inputs in ctxt.inputs.values().chain(ctxt.scheduleds.values()) {
        exprs.push(quote!(#inputs.set(core::mem::uninitialized());))
    }

    // these are `MaybeUninit` `ReadyQueue`s
    for queue in ctxt.ready_queues.values() {
        exprs.push(quote!(#queue.set(rtfm::export::ReadyQueue::new());))
    }

    // these are `MaybeUninit` `FreeQueue`s
    for free in ctxt.free_queues.values() {
        exprs.push(quote!(#free.set(rtfm::export::FreeQueue::new());))
    }

    // end-of-FIXME

    // Initialize the timer queue
    if !analysis.timer_queue.tasks.is_empty() {
        let tq = &ctxt.timer_queue;
        exprs.push(quote!(#tq.set(rtfm::export::TimerQueue::new(p.SYST));));
    }

    // Populate the `FreeQueue`s
    for (task, alias) in &ctxt.free_queues {
        let capacity = analysis.capacities[task];
        exprs.push(quote!(
            for i in 0..#capacity {
                #alias.get_mut().enqueue_unchecked(i);
            }
        ))
    }

    // Set the cycle count to 0 and disable it while `init` executes
    exprs.push(quote!(p.DWT.ctrl.modify(|r| r & !1);));
    exprs.push(quote!(p.DWT.cyccnt.write(0);));

    quote!(
        let mut p = unsafe { rtfm::export::Peripherals::steal() };
        unsafe {
            #(#exprs)*
        }
    )
}

fn assertions(app: &App, analysis: &Analysis) -> proc_macro2::TokenStream {
    let mut items = vec![];

    for ty in &analysis.needs_sync {
        items.push(quote!(rtfm::export::assert_sync::<#ty>()));
    }

    for task in &analysis.needs_send {
        let ty = tuple_ty(&app.tasks[task].inputs);
        items.push(quote!(rtfm::export::assert_send::<#ty>()));
    }

    quote!(#(#items;)*)
}

fn mk_resource(
    ctxt: &Context,
    struct_: &Ident,
    ty: proc_macro2::TokenStream,
    ceiling: u8,
    ptr: proc_macro2::TokenStream,
    app: &App,
) -> proc_macro2::TokenStream {
    let priority = &ctxt.priority;
    let device = &app.args.device;

    quote!(
        struct #struct_<'a> { #priority: &'a core::cell::Cell<u8>}

        unsafe impl<'a> rtfm::Mutex for #struct_<'a> {
            const CEILING: u8 = #ceiling;
            const NVIC_PRIO_BITS: u8 = #device::NVIC_PRIO_BITS;
            type Data = #ty;

            #[inline(always)]
            unsafe fn priority(&self) -> &core::cell::Cell<u8> {
                &self.#priority
            }

            #[inline(always)]
            fn ptr(&self) -> *mut Self::Data {
                unsafe { #ptr }
            }
        }
    )
}

fn mk_capacity_literal(capacity: u8) -> LitInt {
    LitInt::new(u64::from(capacity), IntSuffix::None, Span::call_site())
}

fn mk_typenum_capacity(capacity: u8, power_of_two: bool) -> proc_macro2::TokenStream {
    let capacity = if power_of_two {
        capacity
            .checked_next_power_of_two()
            .expect("capacity.next_power_of_two()")
    } else {
        capacity
    };

    let ident = Ident::new(&format!("U{}", capacity), Span::call_site());

    quote!(rtfm::export::consts::#ident)
}

fn mk_ident() -> Ident {
    static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();

    let secs = elapsed.as_secs();
    let nanos = elapsed.subsec_nanos();

    let count = CALL_COUNT.fetch_add(1, Ordering::SeqCst) as u32;
    let mut seed: [u8; 16] = [0; 16];

    for (i, v) in seed.iter_mut().take(8).enumerate() {
        *v = ((secs >> (i * 8)) & 0xFF) as u8
    }

    for (i, v) in seed.iter_mut().skip(8).take(4).enumerate() {
        *v = ((nanos >> (i * 8)) & 0xFF) as u8
    }

    for (i, v) in seed.iter_mut().skip(12).enumerate() {
        *v = ((count >> (i * 8)) & 0xFF) as u8
    }

    let mut rng = rand::rngs::SmallRng::from_seed(seed);
    Ident::new(
        &(0..16)
            .map(|i| {
                if i == 0 || rng.gen() {
                    ('a' as u8 + rng.gen::<u8>() % 25) as char
                } else {
                    ('0' as u8 + rng.gen::<u8>() % 10) as char
                }
            })
            .collect::<String>(),
        Span::call_site(),
    )
}

// `once = true` means that these locals will be called from a function that will run *once*
fn mk_locals(locals: &HashMap<Ident, Static>, once: bool) -> proc_macro2::TokenStream {
    let lt = if once { Some(quote!('static)) } else { None };

    let locals = locals
        .iter()
        .map(|(name, static_)| {
            let attrs = &static_.attrs;
            let expr = &static_.expr;
            let ident = name;
            let ty = &static_.ty;

            quote!(
                #[allow(non_snake_case)]
                let #ident: &#lt mut #ty = {
                    #(#attrs)*
                    static mut #ident: #ty = #expr;

                    unsafe { &mut #ident }
                };
            )
        })
        .collect::<Vec<_>>();

    quote!(#(#locals)*)
}

fn tuple_pat(inputs: &[ArgCaptured]) -> proc_macro2::TokenStream {
    if inputs.len() == 1 {
        let pat = &inputs[0].pat;
        quote!(#pat)
    } else {
        let pats = inputs.iter().map(|i| &i.pat).collect::<Vec<_>>();

        quote!(#(#pats,)*)
    }
}

fn tuple_ty(inputs: &[ArgCaptured]) -> proc_macro2::TokenStream {
    if inputs.len() == 1 {
        let ty = &inputs[0].ty;
        quote!(#ty)
    } else {
        let tys = inputs.iter().map(|i| &i.ty).collect::<Vec<_>>();

        quote!((#(#tys,)*))
    }
}

#[derive(Clone, PartialEq)]
enum Kind {
    Exception(Ident),
    Idle,
    Init,
    Interrupt(Ident),
    Task(Ident),
}

impl Kind {
    fn ident(&self) -> Ident {
        match self {
            Kind::Init => Ident::new("init", Span::call_site()),
            Kind::Idle => Ident::new("idle", Span::call_site()),
            Kind::Task(name) | Kind::Interrupt(name) | Kind::Exception(name) => name.clone(),
        }
    }

    fn is_idle(&self) -> bool {
        *self == Kind::Idle
    }

    fn is_init(&self) -> bool {
        *self == Kind::Init
    }

    fn runs_once(&self) -> bool {
        match *self {
            Kind::Init | Kind::Idle => true,
            _ => false,
        }
    }
}
