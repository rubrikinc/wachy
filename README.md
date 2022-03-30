# wachy

<img src="docs/images/logo.png?raw=true" alt="Logo" width="72" align="left">

<br>

&nbsp; A dynamic tracing profiler for Linux

<br>

Wachy provides a UI for interactive eBPF-based userspace performance debugging.
For an overview, see the website: https://rubrikinc.github.io/wachy/. For
background see the introductory
[blog post](https://www.rubrik.com/blog/technology/22/1/introducing-wachy-a-new-approach-to-performance-debugging).

For more details see the [guide](docs/guide.md).

## Compatibility

Wachy requires:
1. Linux 4.6 or later kernel
2. Traced binary should be in a compiled language, and have debugging symbols

1 is due to availability of certain eBPF features, and 2 is due to the
techniques used by wachy (eBPF uprobes and address to line number mappings from
debugging symbols). Wachy also supports C++ symbol demangling - it has mostly
been tested with C++ binaries. If you'd like demangling support for a new
compiled language, please open an issue (note: despite being compiled, [Go does
not play well with
eBPF](https://medium.com/bumble-tech/bpf-and-go-modern-forms-of-introspection-in-linux-6b9802682223#db17)).
If you have ideas on how to do something similar on other platforms or with
other unsupported languages, I'm interested in hearing it!

Wachy also currently only supports x86-64 binaries. If you are interested in
other architectures, please open an issue.

## Install

Download the latest version from the [Releases
page](https://github.com/rubrikinc/wachy/releases).

Wachy relies on
[bpftrace](https://github.com/iovisor/bpftrace/blob/master/INSTALL.md) and the
following shared libraries to run: libgcc_s, libncursesw. On ubuntu some of
these may be installed by default, but to install them all you can run
```
sudo apt install bpftrace libgcc1 libncursesw5
```

If you see strange characters in the TUI, ensure your `LANG` is set correctly,
e.g. before starting wachy, run
```
export LANG=en_US.UTF-8
```

## Compiling

If you want to build wachy from source, it requires the following development
packages: libiberty, ncurses, cmake. On ubuntu you can install them with
```
sudo apt install libiberty-dev libncurses5-dev libncursesw5-dev cmake
```
You also need [Rust](https://www.rust-lang.org) installed.

Then build with cargo
```
cargo build --release
target/release/wachy --help
```

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as per [LICENSE.md](LICENSE.md), without any additional terms or
conditions.

Contributions to this project must be accompanied by a Contributor License
Agreement. We use https://cla-assistant.io to automate this process.
