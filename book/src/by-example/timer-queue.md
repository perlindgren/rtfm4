# Timer queue

The RTFM framework includes a *global timer queue* that applications can use to
*schedule* software tasks to run some time in the future.

To be able to schedule a software task the name of the task must appear in the
`schedule` argument of the context attribute. When scheduling a task the
[`Instant`] at which the task should be executed must be passed as the first
argument of the `schedule` invocation.

[`Instant`]: ../../api/rtfm/struct.Instant.html

The RTFM runtime includes a monotonic, non-decreasing, 32-bit timer which can be
queried using the `Instant::now` constructor. A [`Duration`] can be added to
`Instant::now()` to obtain an `Instant` into the future. The monotonic timer is
disabled while `init` runs so `Instant::now()` always returns the value
`Instant(0 /* cycles */)`; the timer is enabled and started right before the
interrupts are re-enabled and `idle` is executed.

[`Duration`]: ../../api/rtfm/struct.Duration.html

The example below schedules two tasks from `init`: `foo` and `bar`. `foo` is
scheduled to run 8 million cycles in the future. Next, `bar` is scheduled to
run 4 million cycles in the future. `bar` runs before `foo` since it was
scheduled to run first.

> **IMPORTANT**: The examples that use the timer queue feature or the `Instant`
> abstraction will *not* properly work on QEMU because the Cortex-M cycle
> counter functionality has not been implemented in `qemu-system-arm`.

``` rust
{{#include ../../../examples/schedule.rs}}
```

Running the program on real hardware produces the following output in the console:

``` text
{{#include ../../../ci/expected/schedule.run}}
```

## Periodic tasks

Software tasks have access to the `Instant` at which they were scheduled to run
through the `scheduled` variable. This information and the `schedule` API can be
used to implement periodic tasks as shown in the example below.

``` rust
{{#include ../../../examples/periodic.rs}}
```

This is the output produced by the example. Note that there is zero drift /
jitter even though `schedule.foo` was invoked at the *end* of `foo`. Using
`Instant::now` instead of `scheduled` would have produced drift / jitter.

``` text
{{#include ../../../ci/expected/periodic.run}}
```

## Baseline

For tasks scheduled from `init` we have exact information about their
`scheduled` time. For hardware tasks there's no `scheduled` time because these
tasks are asynchronous in nature. For hardware task the runtime provides a
`start` time, which indicates the time at which the task handler started
executing.

Note that `start` is **not** equal to the arrival time of the event that fired
the task. Depending on the priority of the task and the load of the system the
`start` time could be very far off from the event arrival time.

What do you think will be the value of `scheduled` for software tasks that are
*spawned* instead of scheduled? The answer is: spawned tasks inherit the
*baseline* (time) of the context that spawned it. The baseline of hardware tasks
is `start`, the baseline of software tasks is `scheduled` and the baseline of
`init` is `Instant(0)`. `idle` doesn't really have a baseline but tasks spawned
from it will use `Instant::now()` as their baseline time.

The example below showcases the different meanings of the *baseline*.

``` rust
{{#include ../../../examples/baseline.rs}}
```

Running the program on real hardware produces the following output in the console:

``` text
{{#include ../../../ci/expected/baseline.run}}
```
