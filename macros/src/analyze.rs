use std::{
    cmp,
    collections::{HashMap, HashSet},
};

use syn::{Attribute, Ident, Type};

use syntax::{App, Idents};

pub type Ownerships = HashMap<Ident, Ownership>;

pub struct Analysis {
    /// Capacities of free queues
    pub capacities: Capacities,
    pub dispatchers: Dispatchers,
    // Ceilings of free queues
    pub free_queues: HashMap<Ident, u8>,
    /// Tasks whose inputs need to be Send
    pub needs_send: HashSet<Ident>,
    /// Types of RO resources that need to be Sync
    pub needs_sync: HashSet<Box<Type>>,
    // Resource ownership
    pub ownerships: Ownerships,
    // Ceilings of ready queues
    pub ready_queues: HashMap<u8, u8>,
    pub timer_queue: TimerQueue,
}

#[derive(Clone, Copy)]
pub enum Ownership {
    // NOTE priorities and ceilings are "logical" (0 = lowest priority, 255 = highest priority)
    Owned { priority: u8 },
    Shared { ceiling: u8 },
}

impl Ownership {
    pub fn needs_lock(&self, priority: u8) -> bool {
        match *self {
            Ownership::Owned { .. } => false,
            Ownership::Shared { ceiling } => {
                debug_assert!(ceiling >= priority);

                priority < ceiling
            }
        }
    }
}

pub struct Dispatcher {
    /// Attributes to apply to the dispatcher
    pub attrs: Vec<Attribute>,
    pub interrupt: Ident,
    /// Tasks dispatched at this priority level
    pub tasks: Vec<Ident>,
    // Queue capacity
    pub capacity: u8,
}

/// Priority -> Dispatcher
pub type Dispatchers = HashMap<u8, Dispatcher>;

pub type Capacities = HashMap<Ident, u8>;

pub fn app(app: &App) -> Analysis {
    // Ceiling analysis of R/W resource and Sync analysis of RO resources
    // (Resource shared by tasks that run at different priorities need to be `Sync`)
    let mut ownerships = Ownerships::new();
    let mut needs_send = HashSet::new();
    let mut needs_sync = HashSet::new();

    for (priority, res) in app.resource_accesses() {
        if let Some(ownership) = ownerships.get_mut(res) {
            match *ownership {
                Ownership::Owned { priority: ceiling } | Ownership::Shared { ceiling } => {
                    if priority != ceiling {
                        *ownership = Ownership::Shared {
                            ceiling: cmp::max(ceiling, priority),
                        };

                        let res = &app.resources[res];
                        if res.mutability.is_none() {
                            needs_sync.insert(res.ty.clone());
                        }
                    }
                }
            }

            continue;
        }

        ownerships.insert(res.clone(), Ownership::Owned { priority });
    }

    // Compute sizes of free queues
    // We assume at most one message per `spawn` / `schedule`
    let mut capacities: Capacities = app.tasks.keys().map(|task| (task.clone(), 0)).collect();
    for (_, task) in app.spawn_calls().chain(app.schedule_calls()) {
        *capacities.get_mut(task).expect("BUG: capacities.get_mut") += 1;
    }

    // Override computed capacities if user specified a capacity in `#[task]`
    for (name, task) in &app.tasks {
        if let Some(cap) = task.args.capacity {
            *capacities.get_mut(name).expect("BUG: capacities.get_mut") = cap;
        }
    }

    // Compute the size of the timer queue
    // Compute the priority of the timer queue, which matches the priority of the highest
    // `schedule`-able task
    let mut tq_capacity = 0;
    let mut tq_priority = 1;
    let mut tq_tasks = Idents::new();
    for (_, task) in app.schedule_calls() {
        tq_capacity += capacities[task];
        tq_priority = cmp::max(tq_priority, app.tasks[task].args.priority);
        tq_tasks.insert(task.clone());
    }

    // Compute dispatchers capacities
    // Determine which tasks are dispatched by which dispatcher
    // Compute the timer queue priority which matches the priority of the highest priority
    // dispatcher
    let mut dispatchers = Dispatchers::new();
    let mut free_interrupts = app.free_interrupts.iter();
    let mut tasks = app.tasks.iter().collect::<Vec<_>>();
    tasks.sort_by(|l, r| l.1.args.priority.cmp(&r.1.args.priority));
    for (name, task) in tasks {
        let dispatcher = dispatchers.entry(task.args.priority).or_insert_with(|| {
            let (name, fi) = free_interrupts
                .next()
                .expect("BUG: not enough free_interrupts");

            Dispatcher {
                attrs: fi.attrs.clone(),
                capacity: 0,
                interrupt: name.clone(),
                tasks: vec![],
            }
        });

        dispatcher.capacity += capacities[name];
        dispatcher.tasks.push(name.clone());
    }

    // All messages sent from `init` need to be `Send`
    for task in app.init.args.spawn.iter().chain(&app.init.args.schedule) {
        needs_send.insert(task.clone());
    }

    // Ceiling analysis of free queues (consumer end point) -- first pass
    // Ceiling analysis of ready queues (producer end point)
    // Also compute more Send-ness requirements
    let mut free_queues: HashMap<_, _> = app.tasks.keys().map(|task| (task.clone(), 0)).collect();
    let mut ready_queues: HashMap<_, _> = dispatchers.keys().map(|level| (*level, 0)).collect();
    for (priority, task) in app.spawn_calls() {
        if let Some(priority) = priority {
            // Users of `spawn` contend for the to-be-spawned task FREE_QUEUE
            let c = free_queues.get_mut(task).expect("BUG: free_queue.get_mut");
            *c = cmp::max(*c, priority);

            let c = ready_queues
                .get_mut(&app.tasks[task].args.priority)
                .expect("BUG: ready_queues.get_mut");
            *c = cmp::max(*c, priority);

            // Send is required when sending messages from a task whose priority doesn't match the
            // priority of the receiving task
            if app.tasks[task].args.priority != priority {
                needs_send.insert(task.clone());
            }
        } else {
            // spawns from `init` are excluded from the ceiling analysis
        }
    }

    // Ceiling analysis of free queues (consumer end point) -- second pass
    // Ceiling analysis of the timer queue
    let mut tq_ceiling = tq_priority;
    for (priority, task) in app.schedule_calls() {
        if let Some(priority) = priority {
            // Users of `schedule` contend for the to-be-spawned task FREE_QUEUE (consumer end point)
            let c = free_queues.get_mut(task).expect("BUG: free_queue.get_mut");
            *c = cmp::max(*c, priority);

            // Users of `schedule` contend for the timer queu
            tq_ceiling = cmp::max(tq_ceiling, priority);
        } else {
            // spawns from `init` are excluded from the ceiling analysis
        }
    }

    Analysis {
        capacities,
        dispatchers,
        free_queues,
        needs_send,
        needs_sync,
        ownerships,
        ready_queues,
        timer_queue: TimerQueue {
            capacity: tq_capacity,
            ceiling: tq_ceiling,
            priority: tq_priority,
            tasks: tq_tasks,
        },
    }
}

pub struct TimerQueue {
    pub capacity: u8,
    pub ceiling: u8,
    pub priority: u8,
    pub tasks: Idents,
}
