# wachy

A dynamic tracing profiler for Linux.
 - Uses eBPF to trace arbitrary binaries and compiled functions at runtime with
   0 modifications
 - Understands your source code to make setting up traces much faster and easier
 - View actual time spent in functions, including common blocking calls like
   mutex/IO/network
 - Add tracing filters at runtime

For more details see the [demo](TODO) and the [guide](docs/guide.md).

## Install

Download the latest version from the [Releases page](TODO).

Wachy relies on the following shared libraries to run: libncursesw, libtinfo,
libgcc_s. On ubuntu these may be installed by default (depending on ubuntu
version), but to be sure you can run
```
sudo apt install libncursesw5 libtinfo5 libgcc1
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
$ cargo build --release
$ target/release/wachy --help
```
