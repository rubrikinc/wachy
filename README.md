# wachy

<img src="docs/images/logo.png?raw=true" alt="Logo" width="72" align="left">

<br>

&nbsp; A dynamic tracing profiler for Linux

<br>

 - Uses eBPF to trace arbitrary binaries and compiled functions at runtime with
   0 modifications
 - Understands your source code to make setting up traces much faster and easier
 - View actual time spent in functions, including common blocking calls like
   mutex/IO/network
 - Add tracing filters at runtime

The best way to understand wachy is to watch this 3 minute demo:

[![Demo video](https://img.youtube.com/vi/L6VyQP-YDgE/0.jpg)](https://www.youtube.com/watch?v=L6VyQP-YDgE "wachy demo")

For more details see the [guide](docs/guide.md).

## Compatibility

Wachy requires:
1. Linux 4.6 or later kernel
2. Traced binary should be in a compiled language

1 is due to availability of certain eBPF features, and 2 due to the techniques
used by wachy (eBPF uprobes and debugging symbols). Wachy also supports C++
symbol demangling (it has mostly been tested with C++ binaries). If you'd like
demangling support for a new compiled language, please open an issue (note:
despite being compiled, [Go does not play well with
eBPF](https://medium.com/bumble-tech/bpf-and-go-modern-forms-of-introspection-in-linux-6b9802682223#db17)).
If you have ideas on how to do something similar on other platforms or with
other unsupported languages, I'm interested in hearing it!

## Install

Download the latest version from the [Releases page](TODO).

Wachy relies on
[bpftrace](https://github.com/iovisor/bpftrace/blob/master/INSTALL.md) and the
following shared libraries to run: libncursesw, libtinfo, libgcc_s. On ubuntu
some of these may be installed by default, but to install them all you can run
```
sudo apt install bpftrace libncursesw5 libtinfo5 libgcc1
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

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms
or conditions.
