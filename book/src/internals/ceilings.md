# Ceiling analysis

## Background
** The Stack Resource Policy (SRP) was introduced by Baker in 1991, addressing preemtive scheduling of systems with shared (multi-unit) resources. **

Cortex-m-rtfm is an implementation of SRP[1] for fixed priority scheduling of systems with single-unit resources. The resource protection mechanism can be efficiently implemented [2] based on compile time (static) ceiling analysis of the application (i.e., the set of resources and tasks).

## Tasks and Resources 

The `app!` macro gives a declaritive specification of the application, allowing compile time analysis and code generation by the underlying procedural macro. As a consequence of this approach the set of tasks and resources is static (thus introducing new tasks and resources during run-time is prohibited). Moreover in the current implmentation, tasks and resources cannot be declared outside of `app!`.

The ceiling analysis tracks the per-task resource dependencies, and assigns for each resource the maximum priority among tasks with access to the resource. To this end, the procedural macro accepts:

| | |
| --- |--- |
| `static mut ID:T = V` | resource declaration |
| `static mut ID:T` | late resource declaration|
| `#[init]` | init function`*` |
| `#[idle(attr)]` | idle function`!` |
| `#[interrupt(attr)]` | interrupt handler/function`*` |
| `#[exception(attr)]` | exception handler/function`*` |
| `#[task(attr)]` | run-to-end task/function`*` |

with the accepted optional attributes `attr`:

| | |
| --- |--- |
| `resouces = [ID, ..]` | array of accessible resources |
| `priority = 1..n` | priority |
| `deadline = T` | deadline |
| `inter_arrival = T` | interarrival time |

where `n` is the maximum priority supported by the hardware.

From an SRP point of view, `interrupt`, `exception` and `task` functions are run-to-completion (`*`) *tasks*, each associated a given or derived *priority* (defaulting to 1 if not else stated). RTFM extend the original SRP model with an `init` task (executing before the system is live) and the non-terminating (`!`) `idle` task (resuming execution when all SRP *tasks* have completed execution).

As `init` is executed before the SRP scheduled system goes live, it is allowed global access to all resources in the system. This is accomplished by executing `init` inside an (interrupt free) atomic section.

For the ceiling analysis, `idle` implicitly assumes the priority 0, and since non-terminating can neither be associated `deadline` nor `inter_arrival` attributes.

## Code generation

After assigning each resource to a ceiling value, code is autamatically generated that implement for each task:

| | |
| --- |--- |
| `resources` | struct, according to the attribute `#[TASK(resources = [ID, ...])]`|
| `spawn_now.ID(ARGS), ...` | function, according to the corresponding attribute (`#[TASK(spawn_now = [ID, ...])])` and the recepient  arguments, `fn ID(ARGS), ...` |
| `spawn_after.ID(ARGS), ...`| function, according to the corresponding attribute (`#[TASK(spawn_after = [ID, ...])])` and the recepient argument(s), `fn ID(ARGS), ...` |

* `resources` being a struct with fields for accessible resource, of type:
  * `&mut T` for an *owned* resource, and
  * `Mutex<T>`, for a *shared* resource
* `TASK` being either `idle`,  `interrupt`, `exception` or `task`
* `spawn_after` requries the Cargo `features = ["timer-queue"]`


