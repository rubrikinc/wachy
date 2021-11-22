# Guide

NOTE: wachy is still rather young. While it should be safe thanks to the
sandboxing of eBPF programs, some functionality/edge cases are not fully
implemented yet. If you run into any problems please open an issue.

- [Starting wachy](#starting-wachy)
  - [Source View](#source-view)
- [Features/Keyboard Shortcuts](#featureskeyboard-shortcuts)
  - [<kbd>x</kbd>: Trace Line](#kbdxkbd-trace-line)
  - [<kbd>X</kbd>: Trace Inlined Function](#kbdxkbd-trace-inlined-function)
  - [<kbd>Enter</kbd>: Push Line Onto Stack](#kbdenterkbd-push-line-onto-stack)
    - [Trace Stack](#trace-stack)
  - [<kbd>></kbd>: Specify Function to Push Onto Stack](#kbdkbd-specify-function-to-push-onto-stack)
  - [<kbd>Esc</kbd>: Pop Function From Stack](#kbdesckbd-pop-function-from-stack)
  - [<kbd>h</kbd>: Histogram](#kbdhkbd-histogram)
  - [<kbd>r</kbd>: Restart Trace](#kbdrkbd-restart-trace)
  - [<kbd>f</kbd>: Filter Function Entry](#kbdfkbd-filter-function-entry)
  - [<kbd>g</kbd>: Filter Function Exit](#kbdgkbd-filter-function-exit)
- [Misc](#misc)
  - [Function matching](#function-matching)
  - [Logging](#logging)

# Starting wachy

To run wachy, simply pass it the binary and [function](#function-matching)
within it that you would like to trace.
```
sudo wachy ./program foo
```
If there are multiple matches for the function it will ask you to select one,
otherwise it will drop into source view.

Wachy will trace the function across all running instances of the binary - this
is how eBPF works. You can add a `pid`-based
[filter](#kbdfkbd-filter-function-entry) if you need to limit to a single
process.

<details>
<summary>Debugging symbols</summary>

The program must have debugging symbols (more specifically, the `.debug_line`
section) for wachy to do its magic. Wachy also supports [debug
links](https://sourceware.org/gdb/onlinedocs/gdb/Separate-Debug-Files.html) -
simply place the separate debug file in the current working directory.
</details>

## Source View

![Overview screenshot](images/overview_screenshot.png?raw=true)

Wachy figures out the source information corresponding to the traced function
and displays it in a TUI. It displays live (since it started the current trace)
the average duration/latency of the function as well as frequency with which
it's called.

<details>
<summary>Caveats</summary>

For functions that are called recursively (directly or indirectly), the way
wachy currently traces them will likely result in inaccurate numbers.
</details>

<details>
<summary>Remote debugging</summary>
If the source file is not available, wachy displays blank lines for the line
numbers that it knows about. This can be handy for remote debugging on a
production system where you don't want to copy over the source code, but can
still compare line numbers against the actual code locally.
</details>

The debugging approach that wachy is primarily designed for is iterative
drilldown of function calls. The features below go into more detail on ways to
do this.

# Features/Keyboard Shortcuts

A short summary of these is displayed with `wachy -h` too.

## <kbd>x</kbd>: Trace Line

Toggle tracing a function call on the current line. Line numbers with a `▶`
character next to them indicate lines corresponding to call instructions, thus
they can be traced. If there are multiple calls on the same line, wachy will ask
to pick one. Currently only one call per line can be traced at a time.

## <kbd>X</kbd>: Trace Inlined Function

(<kbd><kbd>shift</kbd>+<kbd>x</kbd></kbd>) Toggle tracing of an inlined function
call. The trace output will be attached to the currently selected line. Suppose
a call to function `bar()` is inlined. Tracing `bar()` itself is not really
possible or well-defined any more (e.g. due to compiler optimizations). However,
suppose `bar` internally calls `baz()`. The source information for `baz` will
still correspond to `bar`'s location which may be in a different location/file.
Thus wachy cannot show it in the current view. To be able to trace `baz`
(assuming it hasn't itself been inlined), use <kbd>X</kbd>.

## <kbd>Enter</kbd>: Push Line Onto Stack

Push a function call on the current line onto the trace stack.

There are 3 types of function calls:

![Function types](images/function_types.png?raw=true)

1. Indirect/Register call - a function that can change at runtime. This is used
   for e.g. calling function pointers or C++ virtual function calls. Wachy does
   not know which function this corresponds to[^1] so it will ask you to specify
   the function (same as [`>`](#kbdkbd-specify-function-to-push-onto-stack)).
2. Direct call - a specific address/function in the program. Wachy can
   automatically find the corresponding function.
3. Dynamic call - a function in a dynamically linked library. Wachy currently
   supports [tracing](#kbdxkbd-trace-line) such calls but not pushing them onto
   the stack.

### Trace Stack

Wachy enforces the ordering of the trace stack - so if you first trace `foo()`,
then add `bar()` to the trace stack, it will only show calls to `bar()` that
happen while inside `foo()`. However, it does not need to be the immediate
parent function, it just has to be somewhere in the call stack - this allows you
to trace a deeply nested function with
[`>`](#kbdkbd-specify-function-to-push-onto-stack) when desired.

## <kbd>></kbd>: Specify Function to Push Onto Stack

(<kbd><kbd>shift</kbd>+<kbd>.</kbd></kbd> on most keyboards) [Select](#function-matching) any
function in the program to push onto the trace stack. See [Trace
Stack](#trace-stack) for more details.

## <kbd>Esc</kbd>: Pop Function From Stack

Pop the top function from the trace stack. It will return to a view of the
parent frame.

## <kbd>h</kbd>: Histogram

Display a histogram of function latency.

![Histogram](images/histogram.png?raw=true)

## <kbd>r</kbd>: Restart Trace

Clear the current aggregated trace information and restart it from scratch.

## <kbd>f</kbd>: Filter Function Entry

Add a filter on function entry for when the current function should be traced.
Use [bpftrace
syntax](https://github.com/iovisor/bpftrace/blob/master/docs/reference_guide.md#4-uprobeuretprobe-dynamic-tracing-user-level-arguments),
e.g. `arg0` to refer to the first argument (note: the first argument of C++
member functions is the `this` pointer). This filter will be maintained on the
current function even when additional functions are pushed onto the stack.

## <kbd>g</kbd>: Filter Function Exit

Add a filter on function exit for when the current function should be traced.
Use [bpftrace
syntax](https://github.com/iovisor/bpftrace/blob/master/docs/reference_guide.md#4-uprobeuretprobe-dynamic-tracing-user-level-arguments).
Wachy defines a special variable `$duration` that corresponds to the current
function's duration in nanoseconds. This allows for some powerful filtering on
what is displayed, e.g. `$duration > 10000000` will only show traces
corresponding to when the current function was slower than 10ms. This filter
will be maintained on the current function even when additional functions are
pushed onto the stack.

<details>
<summary>Caveats</summary>
The way this works is wachy maintains a counter of the number of exit filters
that have passed. For performance reasons, only on return of the topmost
function, it checks whether the counter is at the expected value, and if so the
current trace is saved to be output. This may cause issues and unexpected or
missing output if a function in the trace stack can be called multiple times. A
simple way to avoid those issues is to only ever define an exit filter on the
topmost function in the stack.
</details>

# Misc

## Function matching

Selecting a function in wachy is always done with fuzzy searching. To search for
an exact substring match, prepend the search string with `=`.

## Logging
To enable logging simply specify the `WACHY_LOG` environment variable and it
will be output to the file `wachy.log`. See [log
spec](https://docs.rs/flexi_logger/0.14.3/flexi_logger/struct.LogSpecification.html)
for more details on the format.
```
sudo WACHY_LOG=wachy=info wachy ./program "foo()"
```

[^1]: Technically wachy could figure it out at runtime with eBPF but this is not
      implemented yet.
